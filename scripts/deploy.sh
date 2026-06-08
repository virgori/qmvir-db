#!/bin/bash

# Local deployment script for QMvir on server 161.97.146.164
set -e

DEPLOY_PATH="/opt/qmvir"
SERVICE_NAME="qmvir"
TARGET="x86_64-unknown-linux-gnu"

echo "🚀 Starting local deployment with cross-compilation..."

# Install cross-compilation tools if not present
if ! command -v x86_64-linux-gnu-gcc &> /dev/null; then
    echo "📦 Installing cross-compilation tools..."
    apt-get update
    apt-get install -y gcc-x86_64-linux-gnu
fi

# Setup cargo config for cross-compilation
mkdir -p .cargo
cat > .cargo/config.toml << 'EOF'
[target.x86_64-unknown-linux-gnu]
linker = "x86_64-linux-gnu-gcc"

[env]
CC_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-gcc"
CXX_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-g++"
AR_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-ar"
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "x86_64-linux-gnu-gcc"
EOF

# Check if binary exists
if [ ! -f "target/$TARGET/release/qmvir" ]; then
    echo "❌ Binary not found. Cross-compiling first..."
    rustup target add $TARGET
    cargo build --release --target $TARGET --no-default-features
fi

echo "📦 Creating deployment directories..."
mkdir -p $DEPLOY_PATH/{bin,logs,config,data}

echo "📦 Deploying cross-compiled binary..."
cp target/$TARGET/release/qmvir $DEPLOY_PATH/bin/
chmod +x $DEPLOY_PATH/bin/qmvir

echo "🔍 Verifying binary..."
file $DEPLOY_PATH/bin/qmvir
ls -la $DEPLOY_PATH/bin/qmvir

echo "⚙️ Setting up configuration..."
cat > $DEPLOY_PATH/config/config.toml << 'EOF'
# QMvir Configuration
bind_address = "0.0.0.0:5432"
data_dir = "/opt/qmvir/data"
log_dir = "/opt/qmvir/logs"
log_level = "info"

[storage]
wal_dir = "/opt/qmvir/data/wal"
snapshot_dir = "/opt/qmvir/data/snapshots"

[index]
hnsw_m = 16
hnsw_m0 = 32
hnsw_ef_construction = 200
hnsw_ef_search = 50

[performance]
batch_size = 1000
parallel_queries = 4
cache_size = "1GB"
EOF

echo "🔧 Setting up systemd service..."
cat > /etc/systemd/system/$SERVICE_NAME.service << 'EOF'
[Unit]
Description=QMvir Database Engine
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/qmvir
ExecStart=/opt/qmvir/bin/qmvir --config /opt/qmvir/config/config.toml
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=qmvir

[Install]
WantedBy=multi-user.target
EOF

echo "🔄 Restarting service..."
systemctl daemon-reload
systemctl enable $SERVICE_NAME
systemctl restart $SERVICE_NAME

echo "⏳ Waiting for service to start..."
sleep 5

echo "📊 Checking service status..."
systemctl is-active $SERVICE_NAME
systemctl status $SERVICE_NAME --no-pager -l

echo "📋 Recent logs:"
journalctl -u $SERVICE_NAME --no-pager -n 20

echo "✅ Deployment completed successfully!"
echo "🌐 Service is running on: 0.0.0.0:5432"
