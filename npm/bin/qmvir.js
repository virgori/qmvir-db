#!/usr/bin/env node
/**
 * qmvir — CLI launcher. Executes the native Rust binary (qm).
 */

"use strict";

const { execFileSync } = require("child_process");
const path = require("path");
const fs = require("fs");
const os = require("os");

const args = process.argv.slice(2);
const BIN_DIR = path.join(__dirname, "native");

function getBinaryName() {
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

  const name = map[`${platform}-${arch}`];
  return name ? name + ext : null;
}

const binName = getBinaryName();
if (!binName) {
  console.error(`Error: Unsupported platform ${os.platform()}-${os.arch()}`);
  process.exit(1);
}

const binPath = path.join(BIN_DIR, binName);

if (!fs.existsSync(binPath)) {
  console.error(
    `Error: QMvir binary not found at ${binPath}\n` +
      "Run: npm rebuild qmvir"
  );
  process.exit(1);
}

try {
  execFileSync(binPath, args, { stdio: "inherit", env: process.env });
} catch (err) {
  process.exit(err.status ?? 1);
}
