//! `qm guide` — built-in usage guide and important notes (offline, no network).

use super::i18n::Lang;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideTopic {
    All,
    Quickstart,
    Backup,
    Studio,
    Cli,
    Notes,
}

impl GuideTopic {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" | "" => Some(Self::All),
            "quickstart" | "start" | "quick" => Some(Self::Quickstart),
            "backup" | "restore" | "dump" => Some(Self::Backup),
            "studio" | "desktop" | "gui" => Some(Self::Studio),
            "cli" | "commands" => Some(Self::Cli),
            "notes" | "caveats" | "warnings" | "luu-y" => Some(Self::Notes),
            _ => None,
        }
    }
}

pub fn run_guide(lang: &Lang, topic: &str) {
    let version = env!("CARGO_PKG_VERSION");
    let Some(topic) = GuideTopic::from_str(topic) else {
        eprintln!("unknown guide topic: {topic}");
        eprintln!("topics: all | quickstart | backup | studio | cli | notes");
        std::process::exit(2);
    };

    let body = match lang {
        Lang::Vi => guide_vi(topic, version),
        Lang::Zht => guide_zht(topic, version),
        Lang::Zh => guide_zh(topic, version),
        Lang::En => guide_en(topic, version),
    };
    print!("{body}");
}

fn guide_en(topic: GuideTopic, version: &str) -> String {
    match topic {
        GuideTopic::All => format!(
            "\
QMvir v{version} — Usage Guide (built-in)
Use --lang en|vi|zht|zh. Topics: qm guide <topic>

TOPICS
  quickstart   Install, start server, first SQL
  backup       .qmvb backup/restore, dump, checkpoint
  studio       QMvir Studio desktop admin app
  cli          QMvir-exclusive CLI commands (not in psql)
  notes        Important caveats and upgrade tips

QUICK START
  npm install -g qmvir          # or download qm-* binary from GitHub releases
  qm start --admin-password pw    # daemon on 127.0.0.1:55433
  psql -h 127.0.0.1 -p 55433 -U admin
  qm --data-dir ./data sql \"SELECT 1\"

ESSENTIAL COMMANDS
  qm status | stop | version
  qm backup -o prod.qmvb --compress zstd
  qm verify prod.qmvb --info
  qm restore -i prod.qmvb --drop-existing
  qm checkpoint
  qm guide notes                # read before production

Full docs: USAGE_GUIDE_EN.md in the source repo
Performance: docs/QMVIR_PERFORMANCE_GUIDE_VI.md
"
        ),
        GuideTopic::Quickstart => format!(
            "\
QMvir v{version} — Quick Start

1) INSTALL
   npm install -g qmvir
   # or: curl -LO .../qm-macos-arm64 && chmod +x && sudo mv qm /usr/local/bin/

2) DATA DIRECTORY
   qm --data-dir ./mydb <command>     # default: ./data

3) START SERVER (PostgreSQL wire protocol)
   export QM_ADMIN_PASSWORD=secret    # optional
   qm --data-dir ./mydb start --admin-password secret
   qm --data-dir ./mydb start --foreground --admin-password secret

4) CONNECT
   psql -h 127.0.0.1 -p 55433 -U admin
   qm --data-dir ./mydb sql \"CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)\"

5) STOP
   qm --data-dir ./mydb stop
   qm --data-dir ./mydb status

6) BENCHMARK (optional)
   qm benchtest --profile quick
"
        ),
        GuideTopic::Backup => format!(
            "\
QMvir v{version} — Backup, Restore & Export

NATIVE BACKUP (.qmvb format v1 — portable across engine upgrades)
  qm --data-dir ./mydb backup -o backup.qmvb
  qm --data-dir ./mydb backup -o backup.qmvb --compress zstd --tables users,orders
  qm --data-dir ./mydb backup -o backup.qmvb --pitr          # include WAL segment
  qm verify backup.qmvb --info
  qm --data-dir ./mydb restore -i backup.qmvb --drop-existing
  qm --data-dir ./mydb restore -i backup.qmvb --tables users

DIFFERENTIAL
  qm --data-dir ./mydb backup -o full.qmvb
  qm --data-dir ./mydb diff-backup -b full.qmvb -o delta.qmdiff
  qm --data-dir ./mydb restore -i full.qmvb
  qm --data-dir ./mydb restore -i delta.qmdiff

ENCRYPTION (AES-256-GCM)
  export QM_ENCRYPT_KEY=your_secret
  qm encrypt backup.qmvb
  qm decrypt backup.qmvb.enc -o backup.qmvb

CHECKPOINT (flush tables + indexes, truncate WAL)
  qm --data-dir ./mydb checkpoint

TABLE EXPORT (not a full backup)
  qm --data-dir ./mydb dump -f csv -t users -o users.csv
  qm --data-dir ./mydb dump -f sql -t users --stdout
  qm --data-dir ./mydb dump -f parquet -t events -o events.parquet

PG_DUMP IMPORT
  qm --data-dir ./mydb restore -i dump.sql --drop-existing   # plain SQL only

UPGRADE PATH
  old engine: qm backup -o snap.qmvb
  new engine: qm restore -i snap.qmvb --drop-existing
"
        ),
        GuideTopic::Studio => format!(
            "\
QMvir v{version} — QMvir Studio (desktop admin)

QMvir Studio is a native desktop app (Tauri + SolidJS) for SQL editing,
schema browsing, CSV import/export, and filesystem backups.

INSTALL (from GitHub releases — tag studio-v*)
  macOS:   open qmvir-studio_*.dmg
  Linux:   sudo dpkg -i qmvir-studio_*.deb
  Windows: run the .msi installer

DEV BUILD (from source)
  cd qmvir-studio && npm install && npm run tauri dev

CONNECT IN STUDIO
  • Local folder: point to the same --data-dir used by `qm start`
  • Remote: host 127.0.0.1, port 55433, user admin (pgwire)

STUDIO vs CLI
  Studio backup  → .tar.gz of the data directory (fast filesystem copy)
  CLI backup     → .qmvb logical backup (portable, CRC/HMAC, selective tables)
  For engine upgrades, prefer: qm backup / qm restore (.qmvb)

WEB DASHBOARD (optional, separate binary)
  cargo build --release --no-default-features --bin qm_web
  qm_web --data-dir ./mydb --host 127.0.0.1 --port 8080 --admin-password pw
  Browser: http://127.0.0.1:8080  (REST API + metrics)
"
        ),
        GuideTopic::Cli => format!(
            "\
QMvir v{version} — CLI-only commands (not available in psql)

GLOBAL
  --data-dir <PATH>     Data directory (default: ./data)
  --lang en|vi|zht|zh   Guide and messages language

SERVER
  qm start | stop | status

SQL & INTROSPECTION
  qm sql \"<SQL>\"
  qm inspect --tables | qm inspect --table <name>
  qm stat [--json] | qm check [--all]

BACKUP SUITE
  qm backup | restore | diff-backup | verify | predict
  qm encrypt | decrypt | checkpoint | dump

SCHEMA
  qm schema export
  qm schema diff <dir_a> <dir_b> [--output mig.sql]
  qm schema migrate <file.sql> [--dry-run]

OTHER
  qm benchtest [--profile quick|standard] [--json]
  qm version
  qm guide [topic]    # this help

TIP: run `qm guide notes` before production deployments.
"
        ),
        GuideTopic::Notes => format!(
            "\
QMvir v{version} — Important notes & caveats

BACKUP CHOICE
  • .qmvb (qm backup)     Portable logical backup; use for upgrades & off-site copy
  • .tar.gz (Studio)      Full data-dir snapshot; restore only to same layout
  • qm dump               Table export only — not a disaster-recovery backup

FORMAT COMPATIBILITY
  • .qmvb / .qmdiff use format v1 (magic QMVB, CRC32 + optional HMAC)
  • Backups from 6.x remain readable on newer 6.x engines that support v1
  • Restoring a backup from a NEWER engine on an OLDER binary fails with an
    explicit message — upgrade QMvir first

SECURITY
  • Set QM_ADMIN_PASSWORD (or --admin-password) before `qm start`
  • QM_ENCRYPT_KEY for `qm encrypt` / `qm decrypt`
  • QM_SNAPSHOT_HMAC_KEY signs backup chunks (verified on restore)

DURABILITY
  • `qm checkpoint` flushes memory state; run before copying ./data manually
  • Use `qm backup --pitr` if you need WAL segments in the backup file
  • Stop server (`qm stop`) before replacing the data directory from a tar copy

PSQL vs QM CLI
  • CREATE/SELECT/DML → psql or `qm sql`
  • backup, restore, dump, checkpoint, benchtest → `qm` only

STUDIO / WEB
  • Studio talks pgwire or opens a local data-dir — keep versions aligned
  • qm_web REST API uses Basic Auth (admin:password)

PERFORMANCE
  • WAL group commit: see performance guide for tuning scripts
  • Vector HNSW: CREATE INDEX ... USING hnsw after bulk INSERT

More: qm guide backup | qm guide studio | USAGE_GUIDE_EN.md
"
        ),
    }
}

fn guide_vi(topic: GuideTopic, version: &str) -> String {
    match topic {
        GuideTopic::All => format!(
            "\
QMvir v{version} — Huong dan su dung (tich hop trong CLI)
Dung --lang vi. Cac chu de: qm guide <topic>

CHU DE
  quickstart   Cai dat, khoi dong server, SQL dau tien
  backup       Sao luu .qmvb, restore, dump, checkpoint
  studio       Ung dung QMvir Studio (desktop)
  cli          Lenh chi co trong qm (khong co trong psql)
  notes        Luu y quan trong truoc production

BAT DAU NHANH
  npm install -g qmvir
  qm start --admin-password matkhau
  psql -h 127.0.0.1 -p 55433 -U admin
  qm --data-dir ./data sql \"SELECT 1\"

LENH THUONG DUNG
  qm status | stop | version
  qm backup -o prod.qmvb --compress zstd
  qm verify prod.qmvb --info
  qm restore -i prod.qmvb --drop-existing
  qm checkpoint
  qm guide notes

Tai lieu day du: USAGE_GUIDE_EN.md (tieng Anh)
"
        ),
        GuideTopic::Notes => format!(
            "\
QMvir v{version} — Luu y quan trong

SAO LUU
  • .qmvb (qm backup)  — portable, dung khi nang cap engine
  • .tar.gz (Studio)   — snapshot thu muc data; khong thay the .qmvb khi migrate
  • qm dump            — chi export bang, khong phai backup disaster recovery

TUONG THICH
  • Dinh dang .qmvb v1 — backup 6.x doc duoc tren 6.x moi hon (cung v1)
  • File backup tu engine MOI HON se bao loi ro rang neu restore tren binary cu

BAO MAT
  • QM_ADMIN_PASSWORD truoc khi qm start
  • QM_ENCRYPT_KEY cho encrypt/decrypt backup

DURABILITY
  • qm checkpoint truoc khi copy thu muc ./data bang tay
  • qm stop truoc khi thay the data dir tu ban tar

PSQL vs QM
  • SQL thong thuong → psql hoac qm sql
  • backup, restore, dump, checkpoint → chi qm CLI

Xem them: qm guide backup | qm guide studio
"
        ),
        _ => guide_en(topic, version),
    }
}

fn guide_zht(topic: GuideTopic, version: &str) -> String {
    match topic {
        GuideTopic::All => format!(
            "\
QMvir v{version} — 使用指南（內建）

主題：qm guide <topic>
  quickstart | backup | studio | cli | notes

快速開始
  qm start --admin-password <密碼>
  psql -h 127.0.0.1 -p 55433 -U admin
  qm guide notes   # 上線前必讀

詳見 USAGE_GUIDE_EN.md
"
        ),
        GuideTopic::Notes => "\
重要提醒
  • .qmvb（qm backup）— 可攜式邏輯備份，升級引擎時請用此格式
  • Studio .tar.gz — 資料目錄快照，不等同於 .qmvb
  • qm checkpoint — 手動複製 data 目錄前請先執行
  • 僅 psql 無法執行 backup / restore / dump — 請用 qm CLI
"
        .to_string(),
        _ => guide_en(topic, version),
    }
}

fn guide_zh(topic: GuideTopic, version: &str) -> String {
    match topic {
        GuideTopic::All => format!(
            "\
QMvir v{version} — 使用指南（内置）

主题：qm guide <topic>
  quickstart | backup | studio | cli | notes

快速开始
  qm start --admin-password <密码>
  psql -h 127.0.0.1 -p 55433 -U admin
  qm guide notes   # 上线前必读
"
        ),
        GuideTopic::Notes => "\
重要提醒
  • .qmvb（qm backup）— 便携式逻辑备份，升级引擎时请用此格式
  • Studio .tar.gz — 数据目录快照，不等同于 .qmvb
  • qm checkpoint — 手动复制 data 目录前请先执行
  • 仅 psql 无法执行 backup / restore / dump — 请用 qm CLI
"
        .to_string(),
        _ => guide_en(topic, version),
    }
}
