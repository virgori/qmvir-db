//! Smoke test: cluster transport ping + WAL entry round-trip wiring.

use qm_engine::cluster::{NodeClient, TransportServer, WalEntry};
use qm_engine::gateway::NativeSqlEngine;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn cluster_transport_ping() {
    let engine = Arc::new(NativeSqlEngine::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = TransportServer::serve_connection(stream, engine).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = NodeClient::new(1, addr);
    assert!(client.ping().await, "ping should succeed");
}

#[tokio::test]
async fn cluster_wal_entry_applies_on_replica() {
    let replica = Arc::new(NativeSqlEngine::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server_engine = Arc::clone(&replica);
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = TransportServer::serve_connection(stream, server_engine).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let sql = "CREATE TABLE ha_smoke (id INTEGER PRIMARY KEY, name TEXT)";
    let entry = WalEntry {
        lsn: 1,
        sql: sql.to_string(),
        checksum: {
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(sql.as_bytes());
            hasher.finalize()
        },
    };

    let client = NodeClient::new(2, addr);
    let ack_lsn = client.send_wal_entry(&entry).await.expect("wal ship");
    assert_eq!(ack_lsn, 1);

    let rows = replica
        .execute("SELECT COUNT(*) FROM ha_smoke")
        .expect("replica count");
    assert_eq!(rows.rows.len(), 1);
}
