//! AES-256-GCM encryption/decryption for `.qmvb` backup files.
//!
//! Wraps a complete `.qmvb` byte stream in an authenticated envelope:
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ Magic:   u64 = 0x514D_454E_4300     │
//! │ Version: u32 = 1                     │
//! │ KDF:     u8  = 1 (Argon2id)          │
//! │ Salt:    [u8; 16]                    │
//! │ Nonce:   [u8; 12]                    │
//! ├──────────────────────────────────────┤
//! │ Encrypted payload (AES-256-GCM)      │
//! │  (includes 16-byte auth tag at end)  │
//! └──────────────────────────────────────┘
//! ```
//!
//! Key derivation: Argon2id(password, salt) → 32-byte key

use std::io;
use std::path::Path;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroize;

/// Encrypted backup magic: "QMENC\0" followed by version byte.
const ENCRYPT_MAGIC: u64 = 0x514D_454E_4300_0001;
/// Header size: magic(8) + version(4) + kdf(1) + salt(16) + nonce(12) = 41 bytes.
const ENCRYPT_HEADER_SIZE: usize = 41;

/// Derive a 32-byte key from password + salt using Argon2id with explicit parameters.
fn derive_key(password: &[u8], salt: &[u8; 16]) -> io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    // M-10: Use explicit Argon2id parameters instead of default.
    let params = Params::new(47_104, 3, 1, Some(32))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Argon2 params: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password, salt, &mut key)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("KDF error: {e}")))?;
    Ok(key)
}

/// Encrypt a `.qmvb` file, writing `<path>.enc`.
///
/// Returns the encrypted file path.
pub fn encrypt_file(input: &Path, password: &str) -> io::Result<String> {
    let plaintext = std::fs::read(input)?;
    let encrypted = encrypt_bytes(&plaintext, password.as_bytes())?;

    let out_path = format!("{}.enc", input.display());
    std::fs::write(&out_path, &encrypted)?;
    Ok(out_path)
}

/// Decrypt a `.qmvb.enc` file (or any encrypted backup file).
///
/// Returns the decrypted `.qmvb` data as bytes.
pub fn decrypt_file(input: &Path, password: &str) -> io::Result<Vec<u8>> {
    let ciphertext = std::fs::read(input)?;
    decrypt_bytes(&ciphertext, password.as_bytes())
}

/// Decrypt and write to an output file.
pub fn decrypt_file_to(input: &Path, output: &Path, password: &str) -> io::Result<()> {
    let plaintext = decrypt_file(input, password)?;
    std::fs::write(output, &plaintext)
}

/// Encrypt raw bytes with AES-256-GCM + Argon2id KDF.
pub fn encrypt_bytes(plaintext: &[u8], password: &[u8]) -> io::Result<Vec<u8>> {
    // Generate random salt and nonce.
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    // Derive key.
    let mut key = derive_key(password, &salt)?;

    // Encrypt.
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("cipher init: {e}")))?;
    // H-06: Zeroize key material after cipher initialization.
    key.zeroize();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("encryption failed: {e}")))?;

    // Build output: header + ciphertext.
    let mut out = Vec::with_capacity(ENCRYPT_HEADER_SIZE + ciphertext.len());

    // Magic (8 bytes).
    out.extend_from_slice(&ENCRYPT_MAGIC.to_le_bytes());
    // Version (4 bytes).
    out.extend_from_slice(&1u32.to_le_bytes());
    // KDF id (1 byte): 1 = Argon2id.
    out.push(1u8);
    // Salt (16 bytes).
    out.extend_from_slice(&salt);
    // Nonce (12 bytes).
    out.extend_from_slice(&nonce_bytes);
    // Encrypted payload (includes GCM auth tag).
    out.extend_from_slice(&ciphertext);

    Ok(out)
}

/// Decrypt bytes that were encrypted with `encrypt_bytes`.
pub fn decrypt_bytes(data: &[u8], password: &[u8]) -> io::Result<Vec<u8>> {
    if data.len() < ENCRYPT_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "encrypted file too short",
        ));
    }

    // Parse header.
    let magic = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if magic != ENCRYPT_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not an encrypted backup (magic: {:#x})", magic),
        ));
    }

    let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
    if version != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported encryption version: {version}"),
        ));
    }

    let kdf = data[12];
    if kdf != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported KDF: {kdf}"),
        ));
    }

    let mut salt = [0u8; 16];
    salt.copy_from_slice(&data[13..29]);
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&data[29..41]);

    let ciphertext = &data[ENCRYPT_HEADER_SIZE..];

    // Derive key.
    let mut key = derive_key(password, &salt)?;

    // Decrypt.
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("cipher init: {e}")))?;
    // H-06: Zeroize key material after cipher initialization.
    key.zeroize();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "decryption failed: wrong password or corrupted data",
        )
    })?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let data = b"Hello QMvir backup data! Tables and rows here.";
        let password = b"test-password-123";

        let encrypted = encrypt_bytes(data, password).unwrap();
        assert_ne!(&encrypted[..], &data[..]);
        assert!(encrypted.len() > data.len());

        let decrypted = decrypt_bytes(&encrypted, password).unwrap();
        assert_eq!(&decrypted, &data[..]);
    }

    #[test]
    fn wrong_password_fails() {
        let data = b"secret data";
        let encrypted = encrypt_bytes(data, b"correct").unwrap();
        let result = decrypt_bytes(&encrypted, b"wrong");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_magic_rejected() {
        let mut bad = vec![0u8; 100];
        bad[0..8].copy_from_slice(&0xBADCAFE_u64.to_le_bytes());
        let result = decrypt_bytes(&bad, b"pass");
        assert!(result.is_err());
    }

    #[test]
    fn too_short_rejected() {
        let result = decrypt_bytes(&[0u8; 10], b"pass");
        assert!(result.is_err());
    }
}
