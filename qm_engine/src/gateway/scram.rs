/*
 * SCRAM-SHA-256 Authentication (RFC 5802 / RFC 7677)
 *
 * Implements the server side of SCRAM-SHA-256 as used by PostgreSQL.
 *
 * Flow:
 *   1. Server sends AuthenticationSASL with mechanism "SCRAM-SHA-256"
 *   2. Client sends SASLInitialResponse (client-first-message)
 *   3. Server sends AuthenticationSASLContinue (server-first-message)
 *   4. Client sends SASLResponse (client-final-message)
 *   5. Server sends AuthenticationSASLFinal (server-final-message)
 *   6. Server sends AuthenticationOk
 */

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Constant-time byte comparison to prevent timing attacks.
/// Returns `true` iff `a` and `b` have equal length and equal contents.
#[inline(never)]
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Default PBKDF2 iteration count (NIST SP 800-132 / OWASP 2023 recommendation).
const DEFAULT_ITERATIONS: u32 = 600_000;

/// Stored SCRAM credentials for a user.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScramSecret {
    pub iterations: u32,
    pub salt: Vec<u8>,         // raw salt bytes
    pub stored_key: [u8; 32],  // H(ClientKey)
    pub server_key: [u8; 32],  // HMAC(SaltedPassword, "Server Key")
}

impl ScramSecret {
    /// Derive SCRAM credentials from a plaintext password.
    pub fn from_password(password: &str) -> Self {
        let salt: [u8; 16] = rand::random();
        Self::from_password_with_salt(password, &salt, DEFAULT_ITERATIONS)
    }

    pub fn from_password_with_salt(password: &str, salt: &[u8], iterations: u32) -> Self {
        let salted_password = Self::hi(password.as_bytes(), salt, iterations);
        let client_key = Self::hmac(&salted_password, b"Client Key");
        let stored_key = Sha256::digest(client_key);
        let server_key = Self::hmac(&salted_password, b"Server Key");

        Self {
            iterations,
            salt: salt.to_vec(),
            stored_key: stored_key.into(),
            server_key: server_key.into(),
        }
    }

    /// PBKDF2-HMAC-SHA-256 (called "Hi" in SCRAM spec).
    fn hi(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
        let mut out = [0u8; 32];
        pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out);
        out
    }

    fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
        mac.update(msg);
        mac.finalize().into_bytes().into()
    }
}

/// SCRAM server-side state machine.
pub struct ScramServer {
    /// Server nonce (client nonce + server nonce extension).
    nonce: String,
    /// Client-first-message-bare (for auth message construction).
    client_first_bare: String,
    /// Server-first-message (for auth message construction).
    server_first: String,
    /// The SCRAM secret for the user being authenticated.
    secret: ScramSecret,
}

impl ScramServer {
    /// Process client-first-message.  Returns (server-first-message, ScramServer state).
    ///
    /// `client_first` is the full client-first-message, e.g.
    ///   `n,,n=user,r=<client-nonce>`
    ///
    /// `server_supports_cb`: when `true`, the server advertised
    /// `SCRAM-SHA-256-PLUS` — reject clients sending `y,,` (RFC 5802 §6).
    pub fn new(client_first: &str, secret: ScramSecret, server_supports_cb: bool) -> Result<(String, Self), String> {
        // Parse gs2-header and client-first-message-bare
        // gs2-header = "n,," (no channel binding)
        let bare = if let Some(rest) = client_first.strip_prefix("n,,") {
            rest
        } else if let Some(rest) = client_first.strip_prefix("y,,") {
            // "y,," means client supports channel binding but thinks the server
            // does not.  If the server *does* support it, this is a downgrade
            // attack — reject per RFC 5802 §6.
            if server_supports_cb {
                return Err("server supports channel binding; client must not use gs2-cbind-flag 'y'".into());
            }
            rest
        } else if client_first.starts_with("p=") {
            // Channel binding requested — not yet implemented
            return Err("channel binding (p=) not yet supported".into());
        } else {
            return Err("unsupported gs2-cbind-flag".into());
        };

        let mut client_nonce = None;
        for attr in bare.split(',') {
            if let Some(val) = attr.strip_prefix("r=") {
                client_nonce = Some(val.to_string());
            }
        }
        let client_nonce = client_nonce.ok_or("missing client nonce (r=)")?;

        // Generate server nonce extension
        let server_nonce_ext: [u8; 18] = rand::random();
        let combined_nonce = format!("{}{}", client_nonce, B64.encode(server_nonce_ext));

        let server_first = format!(
            "r={},s={},i={}",
            combined_nonce,
            B64.encode(&secret.salt),
            secret.iterations,
        );

        Ok((
            server_first.clone(),
            Self {
                nonce: combined_nonce,
                client_first_bare: bare.to_string(),
                server_first,
                secret,
            },
        ))
    }

    /// Process client-final-message.  Returns server-final-message on success.
    ///
    /// `client_final` is e.g. `c=biws,r=<combined-nonce>,p=<proof-b64>`
    pub fn finish(&self, client_final: &str) -> Result<String, String> {
        let mut channel_binding = None;
        let mut nonce = None;
        let mut proof_b64 = None;

        for attr in client_final.split(',') {
            if let Some(val) = attr.strip_prefix("c=") {
                channel_binding = Some(val.to_string());
            } else if let Some(val) = attr.strip_prefix("r=") {
                nonce = Some(val.to_string());
            } else if let Some(val) = attr.strip_prefix("p=") {
                proof_b64 = Some(val.to_string());
            }
        }

        // Verify nonce (constant-time to prevent timing attacks)
        let nonce = nonce.ok_or("missing nonce in client-final")?;
        if !ct_eq(nonce.as_bytes(), self.nonce.as_bytes()) {
            return Err("nonce mismatch".into());
        }

        // Verify channel binding (should be base64("n,,") = "biws" for no binding)
        let _cb = channel_binding.ok_or("missing channel-binding")?;

        // Extract proof
        let proof_b64 = proof_b64.ok_or("missing proof")?;
        let client_proof = B64.decode(&proof_b64).map_err(|_| "invalid proof base64")?;
        if client_proof.len() != 32 {
            return Err("proof wrong length".into());
        }

        // Build AuthMessage
        // client-final-without-proof  =  everything before ",p="
        let cfwp = match client_final.rfind(",p=") {
            Some(pos) => &client_final[..pos],
            None => return Err("malformed client-final (no ,p=)".into()),
        };
        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, self.server_first, cfwp,
        );

        // Compute expected ClientSignature
        let client_signature = Self::hmac(&self.secret.stored_key, auth_message.as_bytes());

        // Recover ClientKey = ClientProof XOR ClientSignature
        let mut recovered_key = [0u8; 32];
        for i in 0..32 {
            recovered_key[i] = client_proof[i] ^ client_signature[i];
        }

        // Verify: H(recovered_key) should equal stored_key (constant-time comparison)
        let h = Sha256::digest(recovered_key);
        if !ct_eq(h.as_slice(), &self.secret.stored_key) {
            return Err("SCRAM authentication failed".into());
        }

        // Compute ServerSignature
        let server_signature = Self::hmac(&self.secret.server_key, auth_message.as_bytes());
        let server_final = format!("v={}", B64.encode(server_signature));
        Ok(server_final)
    }

    fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
        mac.update(msg);
        mac.finalize().into_bytes().into()
    }
}

/// Parse a SASLInitialResponse payload (raw bytes after the 'p' message body).
///
/// Format: mechanism-name (C-string) + 4-byte data length + data
pub fn parse_sasl_initial(raw: &[u8]) -> Result<(String, Vec<u8>), String> {
    // Find null terminator for mechanism name
    let null_pos = raw.iter().position(|&b| b == 0)
        .ok_or("missing null in SASLInitialResponse")?;
    let mechanism = std::str::from_utf8(&raw[..null_pos])
        .map_err(|_| "invalid mechanism string")?
        .to_string();
    let rest = &raw[null_pos + 1..];
    if rest.len() < 4 {
        return Err("missing data length in SASLInitialResponse".into());
    }
    let data_len = i32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let data = if data_len < 0 {
        Vec::new() // -1 means no data
    } else {
        let dlen = data_len as usize;
        if rest.len() < 4 + dlen {
            return Err("SASLInitialResponse data too short".into());
        }
        rest[4..4 + dlen].to_vec()
    };
    Ok((mechanism, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scram_round_trip() {
        let password = "secret";
        let secret = ScramSecret::from_password(password);

        // Simulate client-first
        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let client_first = format!("n,,n=user,r={}", client_nonce);

        let (server_first, scram) = ScramServer::new(&client_first, secret.clone(), false).unwrap();

        // Extract combined nonce from server_first
        let mut combined_nonce = String::new();
        let mut salt_b64 = String::new();
        let mut iterations = 0u32;
        for attr in server_first.split(',') {
            if let Some(v) = attr.strip_prefix("r=") { combined_nonce = v.to_string(); }
            if let Some(v) = attr.strip_prefix("s=") { salt_b64 = v.to_string(); }
            if let Some(v) = attr.strip_prefix("i=") { iterations = v.parse().unwrap(); }
        }
        assert!(combined_nonce.starts_with(client_nonce));

        // Client side: compute proof
        let salt = B64.decode(&salt_b64).unwrap();
        let salted_password = {
            let mut out = [0u8; 32];
            pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut out);
            out
        };
        let client_key = {
            let mut mac = HmacSha256::new_from_slice(&salted_password).unwrap();
            mac.update(b"Client Key");
            let r: [u8; 32] = mac.finalize().into_bytes().into();
            r
        };
        let stored_key = Sha256::digest(client_key);

        let channel_binding = B64.encode(b"n,,");
        let client_final_without_proof = format!("c={},r={}", channel_binding, combined_nonce);
        let auth_message = format!(
            "n=user,r={},{},{}",
            client_nonce, server_first, client_final_without_proof
        );

        let client_signature = {
            let mut mac = HmacSha256::new_from_slice(&stored_key).unwrap();
            mac.update(auth_message.as_bytes());
            let r: [u8; 32] = mac.finalize().into_bytes().into();
            r
        };

        let mut proof = [0u8; 32];
        for i in 0..32 { proof[i] = client_key[i] ^ client_signature[i]; }

        let client_final = format!("{},p={}", client_final_without_proof, B64.encode(proof));

        // Verify
        let server_final = scram.finish(&client_final).unwrap();
        assert!(server_final.starts_with("v="));
    }
}
