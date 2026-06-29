//! TLS for PostgreSQL wire protocol (pgwire).

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

#[derive(Clone, Default)]
pub struct PgTlsConfig {
    pub enabled: bool,
    pub acceptor: Option<TlsAcceptor>,
}

impl PgTlsConfig {
    pub fn from_env() -> Self {
        ensure_crypto_provider();
        let cert = std::env::var("QM_TLS_CERT")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("QM_PG_TLS_CERT").ok().map(PathBuf::from));
        let key = std::env::var("QM_TLS_KEY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("QM_PG_TLS_KEY").ok().map(PathBuf::from));

        let (Some(cert_path), Some(key_path)) = (cert, key) else {
            return Self::default();
        };

        match load_server_config(&cert_path, &key_path) {
            Ok(cfg) => Self {
                enabled: true,
                acceptor: Some(TlsAcceptor::from(Arc::new(cfg))),
            },
            Err(err) => {
                eprintln!("[TLS] pgwire TLS disabled: {err}");
                Self::default()
            }
        }
    }

    /// Load TLS from explicit certificate paths (integration tests / programmatic config).
    pub fn from_paths(cert_path: &Path, key_path: &Path) -> Self {
        ensure_crypto_provider();
        match load_server_config(cert_path, key_path) {
            Ok(cfg) => Self {
                enabled: true,
                acceptor: Some(TlsAcceptor::from(Arc::new(cfg))),
            },
            Err(err) => {
                eprintln!("[TLS] pgwire TLS disabled: {err}");
                Self::default()
            }
        }
    }
}

fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn load_server_config(cert_path: &Path, key_path: &Path) -> io::Result<ServerConfig> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn load_certs(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let mut reader = io::BufReader::new(File::open(path)?);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn load_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let mut reader = io::BufReader::new(File::open(path)?);
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(key) = keys.pop() {
        return Ok(PrivateKeyDer::Pkcs8(key));
    }
    let mut reader = io::BufReader::new(File::open(path)?);
    let mut rsa = rustls_pemfile::rsa_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    rsa.pop()
        .map(PrivateKeyDer::Pkcs1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found"))
}
