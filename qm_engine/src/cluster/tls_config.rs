/*
 * Phase H — optional rustls for inter-node transport.
 */

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, StreamOwned};

fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Clone, Default)]
pub struct ClusterTlsConfig {
    pub enabled: bool,
    pub server: Option<Arc<ServerConfig>>,
    pub client: Option<Arc<ClientConfig>>,
}

impl ClusterTlsConfig {
    pub fn from_env() -> Self {
        ensure_crypto_provider();
        let cert = std::env::var("QM_CLUSTER_TLS_CERT").ok().map(PathBuf::from);
        let key = std::env::var("QM_CLUSTER_TLS_KEY").ok().map(PathBuf::from);
        let ca = std::env::var("QM_CLUSTER_TLS_CA").ok().map(PathBuf::from);

        let (Some(cert_path), Some(key_path)) = (cert, key) else {
            return Self::default();
        };

        let certs = match load_certs(&cert_path) {
            Ok(c) if !c.is_empty() => c,
            _ => return Self::default(),
        };
        let key = match load_key(&key_path) {
            Ok(k) => k,
            Err(_) => return Self::default(),
        };

        let mut root_store = RootCertStore::empty();
        if let Some(ca_path) = ca {
            if let Ok(ca_certs) = load_certs(&ca_path) {
                for c in ca_certs {
                    let _ = root_store.add(c);
                }
            }
        } else {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }

        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .ok()
            .map(Arc::new);

        let client = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );

        Self {
            enabled: server.is_some(),
            server,
            client: Some(client),
        }
    }
}

fn load_certs(path: &std::path::Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn load_key(path: &std::path::Path) -> io::Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key"))
}

/// Blocking transport stream (plain TCP or TLS).
pub enum BlockingStream {
    Plain(TcpStream),
    Tls(StreamOwned<ClientConnection, TcpStream>),
}

impl Read for BlockingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for BlockingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

pub fn connect_blocking(
    addr: SocketAddr,
    tls: &ClusterTlsConfig,
    timeout: Duration,
) -> io::Result<BlockingStream> {
    ensure_crypto_provider();
    let stream = TcpStream::connect_timeout(&addr, timeout)?;

    if !tls.enabled {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        return Ok(BlockingStream::Plain(stream));
    }

    let Some(client) = tls.client.as_ref() else {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        return Ok(BlockingStream::Plain(stream));
    };

    let name = if addr.ip().is_loopback() {
        ServerName::try_from("localhost").map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid loopback server name")
        })?
    } else {
        ServerName::IpAddress(addr.ip().into())
    };
    let conn = ClientConnection::new(Arc::clone(client), name)
        .map_err(rustls_err)?;
    let mut tls_stream = StreamOwned::new(conn, stream);
    tls_stream.sock.set_read_timeout(Some(timeout))?;
    tls_stream.sock.set_write_timeout(Some(timeout))?;
    drive_tls_handshake(&mut tls_stream)?;
    tls_stream.sock.set_read_timeout(Some(timeout))?;
    tls_stream.sock.set_write_timeout(Some(timeout))?;
    Ok(BlockingStream::Tls(tls_stream))
}

fn rustls_err(e: rustls::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

fn drive_tls_handshake(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
) -> io::Result<()> {
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    Ok(())
}
