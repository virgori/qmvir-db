/*
 * Authorization Module — User management, password verification, table-level ACL.
 *
 * Supports SQL commands:
 *   CREATE USER <name> WITH PASSWORD '<pw>'
 *   DROP USER <name>
 *   ALTER USER <name> WITH PASSWORD '<pw>'
 *   GRANT <priv,...> ON <table> TO <user>
 *   REVOKE <priv,...> ON <table> FROM <user>
 *
 * Built-in superuser `admin` is created on first boot.
 */

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Privilege flags (bitfield)
// ---------------------------------------------------------------------------

/// Table-level privileges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Drop,
    All,
}

impl Privilege {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SELECT" => Some(Self::Select),
            "INSERT" => Some(Self::Insert),
            "UPDATE" => Some(Self::Update),
            "DELETE" => Some(Self::Delete),
            "CREATE" => Some(Self::Create),
            "DROP" => Some(Self::Drop),
            "ALL" | "ALL PRIVILEGES" => Some(Self::All),
            _ => None,
        }
    }

    /// Expand `All` into concrete privileges.
    pub fn expand(privs: &HashSet<Privilege>) -> HashSet<Privilege> {
        if privs.contains(&Self::All) {
            [
                Self::Select,
                Self::Insert,
                Self::Update,
                Self::Delete,
                Self::Create,
                Self::Drop,
            ]
            .into_iter()
            .collect()
        } else {
            privs.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// User record
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRecord {
    pub username: String,
    /// SHA-256(password + salt) — legacy cleartext-auth verification.
    pub password_hash: String,
    pub salt: String,
    /// SCRAM-SHA-256 credentials for PostgreSQL wire clients.
    #[serde(default)]
    pub scram_secret: Option<super::scram::ScramSecret>,
    pub is_superuser: bool,
    /// table_name -> set of privileges.  "*" = all tables.
    pub table_privileges: HashMap<String, HashSet<Privilege>>,
}

impl UserRecord {
    fn new(username: &str, password: &str, is_superuser: bool) -> Self {
        let salt = hex::encode(&rand::random::<[u8; 16]>());
        let hash = Self::hash_password(password, &salt);
        Self {
            username: username.to_string(),
            password_hash: hash,
            salt,
            scram_secret: Some(super::scram::ScramSecret::from_password(password)),
            is_superuser,
            table_privileges: HashMap::new(),
        }
    }

    fn hash_password(password: &str, salt: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        hasher.update(salt.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn verify_password(&self, password: &str) -> bool {
        let hash = Self::hash_password(password, &self.salt);
        // constant-time compare to avoid timing attacks
        constant_time_eq(hash.as_bytes(), self.password_hash.as_bytes())
    }

    pub fn scram_secret(&self) -> Option<super::scram::ScramSecret> {
        self.scram_secret.clone()
    }

    fn ensure_scram_secret(&mut self, password: &str) {
        if self.scram_secret.is_none() {
            self.scram_secret = Some(super::scram::ScramSecret::from_password(password));
        }
    }

    pub fn has_privilege(&self, table: &str, priv_needed: Privilege) -> bool {
        if self.is_superuser {
            return true;
        }
        // Check wildcard first
        if let Some(privs) = self.table_privileges.get("*") {
            let expanded = Privilege::expand(privs);
            if expanded.contains(&priv_needed) {
                return true;
            }
        }
        // Check specific table
        if let Some(privs) = self.table_privileges.get(table) {
            let expanded = Privilege::expand(privs);
            if expanded.contains(&priv_needed) {
                return true;
            }
        }
        false
    }
}

/// Constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// AuthManager
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AuthStore {
    users: HashMap<String, UserRecord>,
}

#[derive(Clone)]
pub struct AuthManager {
    store: Arc<RwLock<AuthStore>>,
    data_dir: Option<PathBuf>,
}

impl AuthManager {
    /// Create a new in-memory AuthManager.
    pub fn new() -> Self {
        let mut mgr = Self {
            store: Arc::new(RwLock::new(AuthStore {
                users: HashMap::new(),
            })),
            data_dir: None,
        };
        mgr.ensure_default_admin();
        mgr
    }

    /// Create with persistence.  Loads existing auth snapshot if present.
    pub fn with_data_dir(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).ok();
        let mut mgr = Self {
            store: Arc::new(RwLock::new(AuthStore {
                users: HashMap::new(),
            })),
            data_dir: Some(dir),
        };
        mgr.load_snapshot();
        mgr.ensure_default_admin();
        mgr
    }

    /// Make sure the `admin` superuser always exists.
    fn ensure_default_admin(&mut self) {
        let mut s = self.store.write().unwrap();
        s.users
            .entry("admin".to_string())
            .or_insert_with(|| UserRecord::new("admin", "admin", true));
    }

    // -----------------------------------------------------------------------
    // Authentication
    // -----------------------------------------------------------------------

    /// Check if a user exists.
    pub fn user_exists(&self, username: &str) -> bool {
        self.store.read().unwrap().users.contains_key(username)
    }

    /// Verify username + password.  Returns Ok(()) or Err(reason).
    pub fn authenticate(&self, username: &str, password: &str) -> Result<(), String> {
        let s = self.store.read().unwrap();
        match s.users.get(username) {
            None => Err(format!(
                "password authentication failed for user \"{}\"",
                username
            )),
            Some(u) => {
                if u.verify_password(password) {
                    Ok(())
                } else {
                    Err(format!(
                        "password authentication failed for user \"{}\"",
                        username
                    ))
                }
            }
        }
    }

    /// SCRAM secret for wire authentication.
    pub fn scram_secret_for(&self, username: &str) -> Result<super::scram::ScramSecret, String> {
        let s = self.store.read().unwrap();
        let user = s.users.get(username).ok_or_else(|| {
            format!("password authentication failed for user \"{}\"", username)
        })?;
        user.scram_secret().ok_or_else(|| {
            format!("SCRAM credentials missing for user \"{}\"", username)
        })
    }

    // -----------------------------------------------------------------------
    // Privilege check
    // -----------------------------------------------------------------------

    pub fn check_privilege(
        &self,
        username: &str,
        table: &str,
        priv_needed: Privilege,
    ) -> Result<(), String> {
        let s = self.store.read().unwrap();
        match s.users.get(username) {
            None => Err(format!("user \"{}\" does not exist", username)),
            Some(u) => {
                if u.has_privilege(table, priv_needed) {
                    Ok(())
                } else {
                    Err(format!("permission denied for table \"{}\"", table,))
                }
            }
        }
    }

    /// Check if user is superuser.
    pub fn is_superuser(&self, username: &str) -> bool {
        let s = self.store.read().unwrap();
        s.users
            .get(username)
            .map(|u| u.is_superuser)
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // SQL command handlers
    // -----------------------------------------------------------------------

    /// CREATE USER <name> WITH PASSWORD '<pw>'
    pub fn create_user(&self, username: &str, password: &str) -> Result<String, String> {
        let mut s = self.store.write().unwrap();
        if s.users.contains_key(username) {
            return Err(format!("role \"{}\" already exists", username));
        }
        s.users.insert(
            username.to_string(),
            UserRecord::new(username, password, false),
        );
        drop(s);
        self.save_snapshot();
        Ok("CREATE ROLE".to_string())
    }

    /// DROP USER <name>
    pub fn drop_user(&self, username: &str) -> Result<String, String> {
        if username == "admin" {
            return Err("cannot drop superuser \"admin\"".to_string());
        }
        let mut s = self.store.write().unwrap();
        if s.users.remove(username).is_none() {
            return Err(format!("role \"{}\" does not exist", username));
        }
        drop(s);
        self.save_snapshot();
        Ok("DROP ROLE".to_string())
    }

    /// ALTER USER <name> WITH PASSWORD '<pw>'
    pub fn alter_user_password(
        &self,
        username: &str,
        new_password: &str,
    ) -> Result<String, String> {
        let mut s = self.store.write().unwrap();
        match s.users.get_mut(username) {
            None => Err(format!("role \"{}\" does not exist", username)),
            Some(u) => {
                u.salt = hex::encode(&rand::random::<[u8; 16]>());
                u.password_hash = UserRecord::hash_password(new_password, &u.salt);
                u.scram_secret = Some(super::scram::ScramSecret::from_password(new_password));
                drop(s);
                self.save_snapshot();
                Ok("ALTER ROLE".to_string())
            }
        }
    }

    /// GRANT <privs> ON <table> TO <user>
    pub fn grant(
        &self,
        privs: &[Privilege],
        table: &str,
        username: &str,
    ) -> Result<String, String> {
        let mut s = self.store.write().unwrap();
        match s.users.get_mut(username) {
            None => Err(format!("role \"{}\" does not exist", username)),
            Some(u) => {
                let set = u.table_privileges.entry(table.to_string()).or_default();
                for p in privs {
                    set.insert(*p);
                }
                drop(s);
                self.save_snapshot();
                Ok("GRANT".to_string())
            }
        }
    }

    /// REVOKE <privs> ON <table> FROM <user>
    pub fn revoke(
        &self,
        privs: &[Privilege],
        table: &str,
        username: &str,
    ) -> Result<String, String> {
        let mut s = self.store.write().unwrap();
        match s.users.get_mut(username) {
            None => Err(format!("role \"{}\" does not exist", username)),
            Some(u) => {
                if let Some(set) = u.table_privileges.get_mut(table) {
                    for p in privs {
                        if *p == Privilege::All {
                            set.clear();
                        } else {
                            set.remove(p);
                        }
                    }
                    if set.is_empty() {
                        u.table_privileges.remove(table);
                    }
                }
                drop(s);
                self.save_snapshot();
                Ok("REVOKE".to_string())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    fn snapshot_path(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("auth.snap"))
    }

    fn load_snapshot(&mut self) {
        let path = match self.snapshot_path() {
            Some(p) => p,
            None => return,
        };
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let loaded: AuthStore = match bincode::deserialize(&data) {
            Ok(s) => s,
            Err(_) => match serde_json::from_slice(&data) {
                Ok(s) => s,
                Err(_) => return,
            },
        };
        *self.store.write().unwrap() = loaded;
    }

    fn save_snapshot(&self) {
        let path = match self.snapshot_path() {
            Some(p) => p,
            None => return,
        };
        let s = self.store.read().unwrap();
        let data = match bincode::serialize(&*s) {
            Ok(d) => d,
            Err(_) => return,
        };
        let tmp = path.with_extension("snap.tmp");
        if fs::write(&tmp, &data).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}
