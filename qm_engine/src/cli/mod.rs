//! `qm` CLI module — command definitions and dispatch.

pub mod backup_cmd;
pub mod check;
pub mod cluster;
pub mod dump;
pub mod guide;
pub mod i18n;
pub mod inspect;
pub mod schema;
pub mod server;
pub mod stat;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "qm",
    version = env!("CARGO_PKG_VERSION"),
    about = "QMvir Database Engine CLI",
    long_about = "QMvir — Hybrid AI-Native Database Engine (Rust)\n\
        OLTP + OLAP + Full-Text Search + Vector Search + Cache\n\
        PostgreSQL wire protocol — compatible with psql, JDBC, all PG clients\n\n\
        QUICK START:\n  \
          qm start --admin-password mypass      # Start server (daemon)\n  \
          psql -h 127.0.0.1 -p 55433 -U admin   # Connect via psql\n  \
          qm sql \"SELECT 1\"                      # Execute SQL directly\n  \
          qm stop                                # Stop server\n\n\
        SQL:\n  \
          CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);\n  \
          INSERT INTO users VALUES (1, 'Alice', 'alice@mail.com');\n  \
          SELECT * FROM users WHERE name = 'Alice';\n\n\
        AUTH:\n  \
          CREATE USER bob WITH PASSWORD 'secret123';\n  \
          GRANT ALL ON users TO bob;\n  \
          ALTER USER admin WITH PASSWORD 'new_pass';\n\n\
        INDEX:\n  \
          CREATE INDEX idx_email ON users (email);           -- B+Tree\n  \
          CREATE INDEX idx_ft ON articles (title, body);     -- Full-text\n  \
          CREATE INDEX idx_vec ON docs (embedding)            -- HNSW vector\n  \
              WITH (metric=cosine, m=16, ef_construction=200);\n\n\
        SEARCH:\n  \
          SELECT * FROM articles WHERE title MATCH 'database' TOP 10;\n  \
          LIKEV VEC [0.1, -0.3, ...] IN docs TOP 5;\n  \
          SEARCH HYBRID ON articles LEXICAL 'AI' VECTOR [...] TOP 10;\n\n\
        BENCHMARK:\n  \
          qm benchtest                           # Run full benchmark suite\n  \
          qm benchtest --profile quick           # Quick benchmark (~5s)\n  \
          qm benchtest --profile standard        # Standard (~30s)\n  \
          qm benchtest --json                    # JSON output\n\n\
        BACKUP:\n  \
          qm backup -o data.qmvb --compress zstd\n  \
          qm restore -i data.qmvb\n  \
          qm guide notes                         # usage guide & caveats\n  \
          qm encrypt data.qmvb -p secret\n\n\
        Docs: https://github.com/virgori/qmvir-releases",
    after_help = "EXAMPLES:\n  \
          qm start --admin-password mypass           # Daemon (default)\n  \
          qm start --foreground --admin-password pw  # Foreground mode\n  \
          qm --data-dir ./mydb sql \"SELECT * FROM users\"\n  \
          qm benchtest --profile quick --json\n  \
          qm guide quickstart\n  \
          qm guide notes\n  \
          qm backup -o backup.qmvb --compress zstd\n  \
          qm inspect --tables\n  \
          qm stat --json\n\n\
        ENV:\n  \
          QM_ADMIN_PASSWORD   Admin password\n  \
          QM_ENCRYPT_KEY      AES-256-GCM encryption key\n  \
          QM_LANG             Language: vi, en, zht, zh (default: en)"
)]
pub struct Cli {
    /// Data directory (default: ./data)
    #[arg(long, global = true, default_value = "data")]
    pub data_dir: PathBuf,

    /// Language: vi, en, zht, zh
    #[arg(long, global = true, env = "QM_LANG", default_value = "en")]
    pub lang: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a backup (.qmvb)
    Backup {
        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Compression: none, lz4, zstd
        #[arg(short, long, default_value = "lz4")]
        compress: String,

        /// Only back up specific tables (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tables: Option<Vec<String>>,

        /// Include WAL for point-in-time recovery
        #[arg(long)]
        pitr: bool,
    },

    /// Verify backup integrity
    Verify {
        /// Path to .qmvb file
        #[arg()]
        file: PathBuf,

        /// Show detailed backup metadata
        #[arg(long)]
        info: bool,
    },

    /// Inspect engine internals
    Inspect {
        /// Show table metadata + sample rows
        #[arg(long)]
        table: Option<String>,

        /// List all tables
        #[arg(long)]
        tables: bool,
    },

    /// Engine statistics
    Stat {
        /// JSON output
        #[arg(long)]
        json: bool,
    },

    /// Integrity check
    Check {
        /// Run full integrity scan
        #[arg(long)]
        all: bool,

        /// Check specific table
        #[arg(long)]
        table: Option<String>,
    },

    /// Restore from a backup (.qmvb)
    Restore {
        /// Input .qmvb file path
        #[arg(short, long)]
        input: PathBuf,

        /// Drop existing tables before restoring
        #[arg(long)]
        drop_existing: bool,

        /// Only restore specific tables (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tables: Option<Vec<String>>,
    },

    /// Force a checkpoint (snapshot tables + truncate WAL)
    Checkpoint,

    /// Execute SQL query
    Sql {
        /// SQL statement to execute
        #[arg()]
        query: String,
    },

    /// Streaming data export (sql, csv, jsonl, parquet)
    Dump {
        /// Export format: sql, csv, jsonl, parquet
        #[arg(short, long, default_value = "sql")]
        format: String,

        /// Table to dump (all tables if omitted)
        #[arg(short, long)]
        table: Option<String>,

        /// Output file (stdout if omitted)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Write to stdout (pipe-friendly)
        #[arg(long)]
        stdout: bool,
    },

    /// Schema diff, export, and migration
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },

    /// Estimate backup size and duration
    Predict {
        /// Compression: none, lz4, zstd
        #[arg(short, long, default_value = "lz4")]
        compress: String,

        /// JSON output
        #[arg(long)]
        json: bool,
    },

    /// Encrypt a .qmvb backup file (AES-256-GCM)
    Encrypt {
        /// Input .qmvb file
        #[arg()]
        file: PathBuf,

        /// Encryption password (or set QM_ENCRYPT_KEY env var)
        #[arg(short, long, env = "QM_ENCRYPT_KEY")]
        password: String,
    },

    /// Decrypt a .qmvb.enc backup file
    Decrypt {
        /// Input encrypted file
        #[arg()]
        file: PathBuf,

        /// Output .qmvb file
        #[arg(short, long)]
        output: PathBuf,

        /// Decryption password (or set QM_ENCRYPT_KEY env var)
        #[arg(short, long, env = "QM_ENCRYPT_KEY")]
        password: String,
    },

    /// Differential backup (only rows changed since base backup)
    DiffBackup {
        /// Base .qmvb file to diff against
        #[arg(short, long)]
        base: PathBuf,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Compression: none, lz4, zstd
        #[arg(short, long, default_value = "lz4")]
        compress: String,
    },

    /// Run built-in benchmark suite (engine internals + SQL + vector search)
    #[command(alias = "bench")]
    Benchtest {
        /// Profile: quick (~5s), standard (~30s)
        #[arg(long, default_value = "quick")]
        profile: String,

        /// JSON output
        #[arg(long)]
        json: bool,
    },

    /// Built-in usage guide and important notes
    Guide {
        /// Topic: all | quickstart | backup | studio | cli | notes
        #[arg(default_value = "all")]
        topic: String,
    },

    /// Build info
    Version,

    /// Start the QMvir database server (PostgreSQL wire protocol)
    ///
    /// Runs as daemon by default. Use --foreground to run in terminal.
    Start {
        /// Listen host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Listen port
        #[arg(long, default_value = "55433")]
        port: u16,

        /// Maximum concurrent connections
        #[arg(long, default_value = "1000")]
        max_connections: usize,

        /// Run in foreground (default: daemon/background)
        #[arg(long)]
        foreground: bool,

        /// Unix domain socket path (optional)
        #[arg(long)]
        unix_socket: Option<String>,

        /// Admin password (or set QM_ADMIN_PASSWORD env var)
        #[arg(long, env = "QM_ADMIN_PASSWORD")]
        admin_password: Option<String>,
    },

    /// Stop a running QMvir server
    Stop,

    /// Show status of a running QMvir server
    Status,

    /// Cluster HA status (env-driven topology)
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },
}

#[derive(Subcommand)]
pub enum ClusterAction {
    /// Show cluster config and shard group summary
    Status,
    /// Ping shard / WAL peers (live)
    Health,
    /// Enterprise HA readiness scorecard
    Readiness,
    /// Register shard primary on cluster peers
    Join {
        #[arg(long)]
        shard_id: u32,
        #[arg(long)]
        primary: String,
        #[arg(long, value_delimiter = ',')]
        replicas: Vec<String>,
    },
    /// Remove a shard from cluster peers
    Leave {
        #[arg(long)]
        shard_id: u32,
    },
    /// Show WAL replication lag (LSN)
    Lag,
    /// Export cluster HA Prometheus metrics
    Metrics,
    /// Enterprise HA certification gate (exit 1 if not certified)
    Certify,
}

#[derive(Subcommand)]
pub enum SchemaAction {
    /// Compare schemas of two data directories
    Diff {
        /// First data directory
        #[arg()]
        dir_a: PathBuf,

        /// Second data directory
        #[arg()]
        dir_b: PathBuf,

        /// Output migration SQL to file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Export DDL (CREATE TABLE statements)
    Export,

    /// Apply migration SQL file
    Migrate {
        /// SQL migration file
        #[arg()]
        file: PathBuf,

        /// Validate without executing
        #[arg(long)]
        dry_run: bool,
    },
}
