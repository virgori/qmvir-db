#!/usr/bin/env node
/**
 * postinstall.js — Download the QMvir native Rust binary for this platform.
 *
 * 1. Downloads binary to bin/native/
 * 2. Installs `qm` and `qmvir` symlinks to system PATH
 *    - Tries /usr/local/bin (root) or ~/.local/bin (user)
 *
 * Uses process.stderr so npm always shows output (npm hides stdout by default).
 */

"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execSync } = require("child_process");

const VERSION = require("../package.json").version;
const REPO = "virgori/qmvir-releases";
const NATIVE_DIR = path.join(__dirname, "..", "bin", "native");

/** Write to stderr so npm always shows it */
function log(msg) { process.stderr.write(`[qmvir] ${msg}\n`); }

function getArtifactName() {
  const platform = os.platform();
  const arch = os.arch();
  const ext = platform === "win32" ? ".exe" : "";

  const map = {
    "darwin-arm64": "qm-macos-arm64",
    "darwin-x64": "qm-macos-x86_64",
    "linux-x64": "qm-linux-x86_64",
    "linux-arm64": "qm-linux-aarch64",
    "win32-x64": "qm-windows-x86_64",
    "win32-arm64": "qm-windows-aarch64",
  };

  const key = `${platform}-${arch}`;
  const name = map[key];
  if (!name) {
    log(`⚠ No prebuilt binary for ${key}. Build from source instead.`);
    return null;
  }
  return name + ext;
}

function download(url) {
  return new Promise((resolve, reject) => {
    const get = (u, redirects = 0) => {
      if (redirects > 5) { reject(new Error("Too many redirects")); return; }
      https
        .get(u, { headers: { "User-Agent": "qmvir-npm" } }, (res) => {
          if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
            get(res.headers.location, redirects + 1);
            return;
          }
          if (res.statusCode !== 200) {
            reject(new Error(`HTTP ${res.statusCode} for ${u}`));
            return;
          }
          const total = parseInt(res.headers["content-length"] || "0", 10);
          const chunks = [];
          let received = 0;
          let lastPct = -1;

          res.on("data", (d) => {
            chunks.push(d);
            received += d.length;
            if (total > 0) {
              const pct = Math.floor((received / total) * 100);
              if (pct >= lastPct + 10) {
                lastPct = pct;
                log(`  ↓ ${(received / 1024 / 1024).toFixed(1)}/${(total / 1024 / 1024).toFixed(1)} MB (${pct}%)`);
              }
            }
          });
          res.on("end", () => resolve(Buffer.concat(chunks)));
          res.on("error", reject);
        })
        .on("error", reject);
    };
    get(url);
  });
}

/**
 * Install binary to system PATH so `qm` and `qmvir` work globally.
 * Strategy: try /usr/local/bin first, then ~/.local/bin as fallback.
 */
function installToPath(binaryPath) {
  const isWin = os.platform() === "win32";
  if (isWin) {
    log("  ⚠ Windows: add the binary directory to PATH manually");
    return;
  }

  const names = ["qm", "qmvir"];

  // Strategy 1: /usr/local/bin (works if root or has write permission)
  const systemDir = "/usr/local/bin";
  if (tryInstallDir(binaryPath, systemDir, names)) return;

  // Strategy 2: ~/.local/bin (user-level, common on Linux)
  const userDir = path.join(os.homedir(), ".local", "bin");
  if (tryInstallDir(binaryPath, userDir, names)) {
    // Check if ~/.local/bin is in PATH
    const envPath = process.env.PATH || "";
    if (!envPath.split(":").includes(userDir)) {
      log(`  ⚠ ${userDir} is not in PATH. Add to ~/.bashrc or ~/.profile:`);
      log(`    export PATH="${userDir}:$PATH"`);
    }
    return;
  }

  log("  ⚠ Could not install to PATH. Run manually:");
  log(`    sudo cp ${binaryPath} /usr/local/bin/qm`);
  log(`    sudo cp ${binaryPath} /usr/local/bin/qmvir`);
}

function tryInstallDir(src, dir, names) {
  try {
    fs.mkdirSync(dir, { recursive: true });
    for (const name of names) {
      const dest = path.join(dir, name);
      try { fs.unlinkSync(dest); } catch {}
      fs.copyFileSync(src, dest);
      fs.chmodSync(dest, 0o755);
    }
    log(`✓ Installed to ${dir}/qm and ${dir}/qmvir`);
    warnRunningProcesses();
    return true;
  } catch {
    return false;
  }
}

/**
 * Fix 5.3: Warn if a qmvir/qm server process is currently running with an older binary.
 * The running process won't auto-restart — the user must restart it manually.
 */
function warnRunningProcesses() {
  if (os.platform() === "win32") return;
  try {
    const pids = execSync("pgrep -x qmvir 2>/dev/null || pgrep -x qm 2>/dev/null || true")
      .toString().trim().split("\n").filter(Boolean);
    if (pids.length > 0) {
      log(`⚠  Running qmvir process(es) detected (PID: ${pids.join(", ")})`);
      log(`   The new binary is installed but the RUNNING server still uses the old binary.`);
      log(`   Restart the server to pick up the new version:`);
      log(`     qmvir restart   OR   kill ${pids[0]} && qmvir start`);
    }
  } catch {
    // pgrep not available — ignore
  }
}

async function main() {
  const artifact = getArtifactName();
  if (!artifact) return;

  // Check if binary is already bundled in the package
  fs.mkdirSync(NATIVE_DIR, { recursive: true });
  const dest = path.join(NATIVE_DIR, artifact);
  if (fs.existsSync(dest)) {
    const stat = fs.statSync(dest);
    if (stat.size > 1024) {
      log(`✓ Binary already bundled: ${artifact} (${(stat.size / 1024 / 1024).toFixed(1)} MB)`);
      installToPath(dest);
      return;
    }
  }

  // Fallback: download from GitHub releases
  const tag = `v${VERSION}`;
  const url = `https://github.com/${REPO}/releases/download/${tag}/${artifact}`;

  log(`Downloading ${artifact} (${tag})...`);

  try {
    const buffer = await download(url);

    fs.writeFileSync(dest, buffer);
    fs.chmodSync(dest, 0o755);

    const sizeMB = (buffer.length / 1024 / 1024).toFixed(1);
    log(`✓ Downloaded ${artifact} (${sizeMB} MB)`);

    // Install to system PATH
    installToPath(dest);
  } catch (err) {
    log(`⚠ Download failed: ${err.message}`);
    log(`  Build from source: cd qm_engine && cargo build --release --bin qm`);
    // Non-fatal: the npx/node_modules/.bin launcher still works if binary is added later
  }
}

main().catch((err) => {
  log(`✗ postinstall failed: ${err.message}`);
  process.exit(1);
});
