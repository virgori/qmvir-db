#!/usr/bin/env bash
# Generate dev TLS certs for QM cluster inter-node TLS (local certify / staging).
set -euo pipefail

OUT_DIR="${1:-./cluster-tls-dev}"
mkdir -p "$OUT_DIR"

if command -v openssl >/dev/null 2>&1; then
  OPENSSL_CNF="$OUT_DIR/openssl.cnf"
  cat >"$OPENSSL_CNF" <<'EOF'
[req]
distinguished_name = req_distinguished_name
x509_extensions = v3_req
prompt = no

[req_distinguished_name]
CN = localhost

[v3_req]
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
IP.1 = 127.0.0.1
EOF
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$OUT_DIR/cluster-dev.key" \
    -out "$OUT_DIR/cluster-dev.crt" \
    -days 3650 \
    -config "$OPENSSL_CNF" \
    -extensions v3_req 2>/dev/null
  echo "Wrote $OUT_DIR/cluster-dev.crt"
  echo "Wrote $OUT_DIR/cluster-dev.key"
  echo ""
  echo "export QM_CLUSTER_TLS_CERT=$OUT_DIR/cluster-dev.crt"
  echo "export QM_CLUSTER_TLS_KEY=$OUT_DIR/cluster-dev.key"
  echo "export QM_CLUSTER_TLS_CA=$OUT_DIR/cluster-dev.crt"
else
  echo "openssl not found — use cargo test cluster_certify_live (rcgen) or install openssl"
  exit 1
fi
