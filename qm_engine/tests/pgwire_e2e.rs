//! pgwire e2e: binary result format, COPY FROM STDIN, TLS (+ psql when available).

use bytes::{Buf, BufMut, BytesMut};
use qm_engine::gateway::{ConnectionConfig, NativeSqlEngine, PgTlsConfig, QueryHandler, Server};
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use std::io::Cursor;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

fn engine_handler(engine: Arc<NativeSqlEngine>) -> QueryHandler {
    Arc::new(move |sql: String| engine.execute(&sql).map_err(|e| e))
}

async fn spawn_server(
    engine: Arc<NativeSqlEngine>,
    tls: Option<PgTlsConfig>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let handler = engine_handler(Arc::clone(&engine));
    let config = ConnectionConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        max_connections: 32,
        ..Default::default()
    };
    let server = Arc::new(match tls {
        Some(tls) => Server::new_with_tls(config, handler, tls),
        None => Server::new(config, handler),
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let server_bg = Arc::clone(&server);
    let handle = tokio::spawn(async move {
        let _ = server_bg.run_with_startup_signal(Some(tx)).await;
    });
    let addr = tokio::task::spawn_blocking(move || rx.recv())
        .await
        .expect("join")
        .expect("startup channel")
        .expect("bind");
    tokio::time::sleep(Duration::from_millis(40)).await;
    (addr, handle)
}

fn write_dev_tls(dir: &std::path::Path) -> (PathBuf, PathBuf, Vec<u8>) {
    use rcgen::{CertificateParams, KeyPair, SanType};
    use std::net::{IpAddr, Ipv4Addr};

    let key_pair = KeyPair::generate().expect("tls key");
    let mut params = CertificateParams::new(vec!["localhost".to_string()]).expect("params");
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    let cert = params.self_signed(&key_pair).expect("cert");
    let cert_pem = cert.pem();
    let cert_path = dir.join("pgwire-e2e.crt");
    let key_path = dir.join("pgwire-e2e.key");
    std::fs::write(&cert_path, &cert_pem).expect("write cert");
    std::fs::write(&key_path, key_pair.serialize_pem()).expect("write key");
    let cert_der: Vec<u8> = cert_der_from_pem(&cert_pem);
    (cert_path, key_path, cert_der)
}

fn cert_der_from_pem(pem: &str) -> Vec<u8> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader).collect();
    certs
        .into_iter()
        .next()
        .expect("one cert")
        .expect("parse cert")
        .to_vec()
}

fn put_cstring(buf: &mut BytesMut, s: &str) {
    buf.put_slice(s.as_bytes());
    buf.put_u8(0);
}

fn encode_startup(user: &str, database: &str) -> Vec<u8> {
    let mut body = BytesMut::new();
    body.put_i32(196608);
    for (k, v) in [("user", user), ("database", database), ("client_encoding", "UTF8")] {
        put_cstring(&mut body, k);
        put_cstring(&mut body, v);
    }
    body.put_u8(0);
    let mut msg = BytesMut::new();
    msg.put_i32(body.len() as i32 + 4);
    msg.put(body);
    msg.to_vec()
}

fn encode_frontend(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(msg_type);
    out.extend_from_slice(&((body.len() as i32) + 4).to_be_bytes());
    out.extend_from_slice(body);
    out
}

enum Io {
    Plain(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl Io {
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Io::Plain(s) => s.write_all(buf).await,
            Io::Tls(s) => s.write_all(buf).await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Io::Plain(s) => s.read(buf).await,
            Io::Tls(s) => s.read(buf).await,
        }
    }
}

struct PgClient {
    io: Io,
    read_buf: Vec<u8>,
}

impl PgClient {
    async fn connect_plain(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self {
            io: Io::Plain(stream),
            read_buf: Vec::new(),
        })
    }

    async fn connect_tls(addr: SocketAddr, cert_der: &[u8]) -> std::io::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut root_store = RootCertStore::empty();
        root_store
            .add(CertificateDer::from(cert_der.to_vec()))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(&8i32.to_be_bytes()).await?;
        stream
            .write_all(&80877103i32.to_be_bytes())
            .await?;
        let mut resp = [0u8; 1];
        stream.read_exact(&mut resp).await?;
        if resp[0] != b'S' {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "server rejected SSL",
            ));
        }
        let stream = connector.connect("localhost".try_into().unwrap(), stream).await?;
        Ok(Self {
            io: Io::Tls(stream),
            read_buf: Vec::new(),
        })
    }

    async fn send(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.io.write_all(data).await
    }

    async fn fill_buf(&mut self) -> std::io::Result<()> {
        let mut tmp = [0u8; 4096];
        let n = self.io.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        self.read_buf.extend_from_slice(&tmp[..n]);
        Ok(())
    }

    async fn read_until_message(&mut self, msg_type: u8) -> std::io::Result<Vec<u8>> {
        loop {
            if let Some(end) = find_message_end(&self.read_buf, msg_type) {
                let out = self.read_buf[..=end].to_vec();
                self.read_buf.drain(..=end);
                return Ok(out);
            }
            self.fill_buf().await?;
        }
    }

    async fn read_until_ready(&mut self) -> std::io::Result<Vec<u8>> {
        self.read_until_message(b'Z').await
    }

    async fn startup(&mut self) -> std::io::Result<Vec<u8>> {
        self.send(&encode_startup("qm", "qm")).await?;
        self.read_until_ready().await
    }

    async fn simple_query(&mut self, sql: &str) -> std::io::Result<Vec<u8>> {
        let mut body = BytesMut::new();
        put_cstring(&mut body, sql);
        self.send(&encode_frontend(b'Q', &body)).await?;
        self.read_until_ready().await
    }

    async fn extended_query(&mut self, frames: &[Vec<u8>]) -> std::io::Result<Vec<u8>> {
        for frame in frames {
            self.send(frame).await?;
        }
        self.read_until_ready().await
    }
}

fn find_message_end(buf: &[u8], msg_type: u8) -> Option<usize> {
    let mut i = 0;
    while i + 5 <= buf.len() {
        let tag = buf[i];
        let len = i32::from_be_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]) as usize;
        if len < 4 || i + 1 + len > buf.len() {
            return None;
        }
        if tag == msg_type {
            return Some(i + len);
        }
        i += 1 + len;
    }
    None
}

fn parse_data_rows(payload: &[u8]) -> Vec<Vec<Option<Vec<u8>>>> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        if payload[i] != b'D' {
            i += 1;
            continue;
        }
        if i + 5 > payload.len() {
            break;
        }
        let len = i32::from_be_bytes([
            payload[i + 1],
            payload[i + 2],
            payload[i + 3],
            payload[i + 4],
        ]) as usize;
        if i + 1 + len > payload.len() {
            break;
        }
        let mut cur = Cursor::new(&payload[i + 5..i + 1 + len]);
        let ncols = cur.get_i16() as usize;
        let mut row = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let flen = cur.get_i32();
            if flen < 0 {
                row.push(None);
            } else {
                let mut data = vec![0u8; flen as usize];
                cur.copy_to_slice(&mut data);
                row.push(Some(data));
            }
        }
        rows.push(row);
        i += 1 + len;
    }
    rows
}

fn message_types(payload: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 5 <= payload.len() {
        let tag = payload[i];
        let len = i32::from_be_bytes([
            payload[i + 1],
            payload[i + 2],
            payload[i + 3],
            payload[i + 4],
        ]) as usize;
        if len < 4 || i + 1 + len > payload.len() {
            break;
        }
        types.push(tag);
        i += 1 + len;
    }
    types
}

fn psql_available() -> bool {
    Command::new("psql")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_psql_blocking(
    addr: SocketAddr,
    cert_path: Option<&std::path::Path>,
    sql: &str,
) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new("psql");
    cmd.arg("-h")
        .arg("127.0.0.1")
        .arg("-p")
        .arg(addr.port().to_string())
        .arg("-U")
        .arg("qm")
        .arg("-d")
        .arg("qm")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-tA")
        .arg("-c")
        .arg(sql)
        .env("PGCONNECT_TIMEOUT", "5");
    if let Some(cert) = cert_path {
        cmd.env("PGSSLMODE", "require");
        cmd.env("PGSSLROOTCERT", cert.to_path_buf());
    } else {
        cmd.env("PGSSLMODE", "disable");
    }
    cmd.output()
}

async fn run_psql(
    addr: SocketAddr,
    cert_path: Option<PathBuf>,
    sql: &str,
) -> std::io::Result<std::process::Output> {
    let sql = sql.to_string();
    tokio::task::spawn_blocking(move || {
        run_psql_blocking(addr, cert_path.as_deref(), &sql)
    })
    .await
    .expect("psql join")
}

#[tokio::test]
async fn e2e_binary_result_format_int4() {
    let engine = Arc::new(NativeSqlEngine::new());
    engine
        .execute("CREATE TABLE bin_res (id INTEGER PRIMARY KEY)")
        .expect("ddl");
    engine
        .execute("INSERT INTO bin_res (id) VALUES (7), (42)")
        .expect("insert");

    let (addr, _handle) = spawn_server(Arc::clone(&engine), None).await;
    let mut client = PgClient::connect_plain(addr).await.expect("connect");
    client.startup().await.expect("startup");

    let mut parse = BytesMut::new();
    put_cstring(&mut parse, "");
    put_cstring(&mut parse, "SELECT id FROM bin_res ORDER BY id");
    parse.put_i16(0);

    let mut bind = BytesMut::new();
    put_cstring(&mut bind, "");
    put_cstring(&mut bind, "");
    bind.put_i16(0);
    bind.put_i16(0);
    bind.put_i16(1);
    bind.put_i16(1);

    let mut describe = BytesMut::new();
    describe.put_u8(b'P');
    put_cstring(&mut describe, "");

    let mut execute = BytesMut::new();
    put_cstring(&mut execute, "");
    execute.put_i32(0);

    client
        .extended_query(&[
            encode_frontend(b'P', &parse),
            encode_frontend(b'B', &bind),
            encode_frontend(b'D', &describe),
            encode_frontend(b'S', &[]),
        ])
        .await
        .expect("describe");
    let resp = client
        .extended_query(&[encode_frontend(b'E', &execute), encode_frontend(b'S', &[])])
        .await
        .expect("execute");

    let rows = parse_data_rows(&resp);
    assert_eq!(rows.len(), 2);
    let first = rows[0][0].as_ref().expect("non-null");
    assert_eq!(first.len(), 8);
    assert_eq!(i64::from_be_bytes(first[..8].try_into().unwrap()), 7);
    let second = rows[1][0].as_ref().expect("non-null");
    assert_eq!(i64::from_be_bytes(second[..8].try_into().unwrap()), 42);
}

#[tokio::test]
async fn e2e_copy_from_stdin_wire() {
    let engine = Arc::new(NativeSqlEngine::new());
    engine
        .execute("CREATE TABLE copy_wire (id INTEGER PRIMARY KEY, v TEXT)")
        .expect("ddl");

    let (addr, _handle) = spawn_server(engine, None).await;
    let mut client = PgClient::connect_plain(addr).await.expect("connect");
    client.startup().await.expect("startup");

    client
        .send(&encode_frontend(
            b'Q',
            &{
                let mut b = BytesMut::new();
                put_cstring(&mut b, "COPY copy_wire FROM STDIN");
                b.to_vec()
            },
        ))
        .await
        .expect("copy start");

    let phase1 = client.read_until_message(b'G').await.expect("copy in");
    assert!(phase1.contains(&b'G'));

    client
        .send(&encode_frontend(b'd', b"1\thello\n2\tworld\n"))
        .await
        .expect("copy data");
    client
        .send(&encode_frontend(b'c', &[]))
        .await
        .expect("copy done");

    let phase2 = client.read_until_ready().await.expect("copy complete");
    let types = message_types(&phase2);
    assert!(
        types.contains(&b'C'),
        "expected CommandComplete, got {:?} payload={:?}",
        types,
        String::from_utf8_lossy(&phase2)
    );
}

#[tokio::test]
async fn e2e_tls_handshake_and_query() {
    let tls_dir = tempfile::tempdir().expect("tmpdir");
    let (cert_path, key_path, cert_der) = write_dev_tls(tls_dir.path());
    let tls = PgTlsConfig::from_paths(&cert_path, &key_path);
    assert!(tls.enabled, "tls must load");

    let engine = Arc::new(NativeSqlEngine::new());
    let (addr, _handle) = spawn_server(engine, Some(tls)).await;

    let mut client = PgClient::connect_tls(addr, &cert_der)
        .await
        .expect("tls connect");
    client.startup().await.expect("startup");
    let resp = client
        .simple_query("SELECT 1 AS one")
        .await
        .expect("query");
    assert!(message_types(&resp).contains(&b'D'));
}

#[tokio::test]
#[ignore = "manual: prints port and holds server for psql debugging"]
async fn e2e_debug_hold_for_psql() {
    let engine = Arc::new(NativeSqlEngine::new());
    let (addr, _handle) = spawn_server(engine, None).await;
    eprintln!("PGPORT={} PGHOST=127.0.0.1", addr.port());
    tokio::time::sleep(Duration::from_secs(300)).await;
}

#[tokio::test]
async fn e2e_gssenc_probe_then_startup() {
    let engine = Arc::new(NativeSqlEngine::new());
    let (addr, _handle) = spawn_server(engine, None).await;
    let mut s = TcpStream::connect(addr).await.expect("connect");
    s.write_all(&8i32.to_be_bytes()).await.expect("gss len");
    s.write_all(&80877104i32.to_be_bytes()).await.expect("gss");
    let mut resp = [0u8; 1];
    s.read_exact(&mut resp).await.expect("gss resp");
    assert_eq!(resp[0], b'N');

    let mut body = BytesMut::new();
    body.put_i32(196608);
    for (k, v) in [
        ("user", "qm"),
        ("database", "qm"),
        ("application_name", "psql"),
        ("client_encoding", "UTF8"),
        ("extra_float_digits", "3"),
    ] {
        put_cstring(&mut body, k);
        put_cstring(&mut body, v);
    }
    body.put_u8(0);
    let mut startup = BytesMut::new();
    startup.put_i32(body.len() as i32 + 4);
    startup.put(body);

    s.write_all(&startup).await.expect("startup");
    let mut buf = vec![0u8; 4096];
    let n = s.read(&mut buf).await.expect("read auth");
    assert!(n > 0, "expected auth response");
    assert!(buf[..n].contains(&b'Z'), "expected ReadyForQuery in {:?}", &buf[..n]);
}

#[tokio::test]
async fn e2e_psql_plain_query() {
    if !psql_available() {
        eprintln!("skip e2e_psql_plain_query: psql not in PATH");
        return;
    }

    let engine = Arc::new(NativeSqlEngine::new());
    let (addr, _handle) = spawn_server(engine, None).await;
    std::net::TcpStream::connect(addr).expect("tcp port should accept connections");

    let out = run_psql(addr, None, "SELECT 42").await.expect("psql plain");
    assert!(
        out.status.success(),
        "psql plain failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

#[tokio::test]
async fn e2e_psql_tls_and_copy() {
    if !psql_available() {
        eprintln!("skip e2e_psql_tls_and_copy: psql not in PATH");
        return;
    }

    let tls_dir = tempfile::tempdir().expect("tmpdir");
    let (cert_path, key_path, _cert_der) = write_dev_tls(tls_dir.path());
    let tls = PgTlsConfig::from_paths(&cert_path, &key_path);
    assert!(tls.enabled);

    let engine = Arc::new(NativeSqlEngine::new());
    engine
        .execute("CREATE TABLE psql_copy (id INTEGER PRIMARY KEY, v TEXT)")
        .expect("ddl");
    let (addr, _handle) = spawn_server(engine, Some(tls)).await;

    let out = run_psql(addr, Some(cert_path.clone()), "SELECT 1")
        .await
        .expect("psql tls");
    assert!(
        out.status.success(),
        "psql tls failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1");

    let port = addr.port();
    let cert = cert_path.clone();
    let out = tokio::task::spawn_blocking(move || {
        let script = format!(
            r#"psql -h 127.0.0.1 -p {port} -U qm -d qm -v ON_ERROR_STOP=1 -f - <<'EOF'
COPY psql_copy (id, v) FROM STDIN;
10	psql_a
20	psql_b
\.
EOF"#
        );
        Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env("PGCONNECT_TIMEOUT", "5")
            .env("PGSSLMODE", "require")
            .env("PGSSLROOTCERT", &cert)
            .output()
    })
    .await
    .expect("join")
    .expect("psql copy");
    assert!(
        out.status.success(),
        "psql COPY failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let count = run_psql(addr, Some(cert_path.clone()), "SELECT COUNT(*) FROM psql_copy")
        .await
        .expect("count");
    assert!(count.status.success());
    let got = String::from_utf8_lossy(&count.stdout).trim().to_string();
    assert!(
        got.parse::<i64>().unwrap_or(0) >= 2,
        "expected at least 2 rows after COPY, got {got}"
    );
    let ids = run_psql(addr, Some(cert_path), "SELECT id FROM psql_copy ORDER BY id")
        .await
        .expect("ids");
    let id_text = String::from_utf8_lossy(&ids.stdout);
    assert!(id_text.contains("10"), "missing id 10: {id_text}");
    assert!(id_text.contains("20"), "missing id 20: {id_text}");
}
