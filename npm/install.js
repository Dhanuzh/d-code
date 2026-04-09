#!/usr/bin/env node
/**
 * d-code installer
 *
 * Downloads the correct pre-built binary from GitHub Releases for the
 * current platform and places it at npm/bin/d-code (or d-code.exe on Windows).
 *
 * Supported targets:
 *   linux   x64  → x86_64-unknown-linux-musl
 *   linux   arm64→ aarch64-unknown-linux-musl
 *   darwin  x64  → x86_64-apple-darwin
 *   darwin  arm64→ aarch64-apple-darwin
 *   win32   x64  → x86_64-pc-windows-msvc
 */

const https = require("https");
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execSync } = require("child_process");

const REPO = "Dhanuzh/d-code";

const TARGETS = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc",
};

const platform = os.platform();
const arch = os.arch();
const key = `${platform}-${arch}`;
const target = TARGETS[key];

if (!target) {
  console.error(
    `[d-code] Unsupported platform: ${platform}-${arch}.\n` +
      `Supported: ${Object.keys(TARGETS).join(", ")}\n\n` +
      `You can build from source:\n  cargo install --git https://github.com/${REPO}`
  );
  process.exit(1);
}

const isWindows = platform === "win32";
const binaryName = isWindows ? "d-code.exe" : "d-code";
const archiveName = isWindows
  ? `d-code-${target}.zip`
  : `d-code-${target}.tar.gz`;
// Always use latest release — no need to keep npm version in sync with GitHub releases.
const downloadUrl = `https://github.com/${REPO}/releases/latest/download/${archiveName}`;
const binDir = path.join(__dirname, "bin");
// Binary is saved as "d-code" (overwrites the JS placeholder shim).
// The npm symlink already points here, so it works automatically after install.
const binaryPath = path.join(binDir, binaryName);

// If already a real binary (> 100KB), skip download.
if (fs.existsSync(binaryPath) && fs.statSync(binaryPath).size > 100_000) {
  try {
    fs.chmodSync(binaryPath, 0o755);
  } catch (_) {}
  console.log(`[d-code] Binary already present at ${binaryPath}`);
  process.exit(0);
}

fs.mkdirSync(binDir, { recursive: true });

console.log(`[d-code] Downloading ${archiveName} from GitHub Releases…`);
console.log(`         ${downloadUrl}`);

downloadAndExtract(downloadUrl, archiveName, binaryName, binaryPath)
  .then(() => {
    fs.chmodSync(binaryPath, 0o755);
    console.log(`[d-code] Installed to ${binaryPath}`);
  })
  .catch((err) => {
    console.error(`[d-code] Installation failed: ${err.message}`);
    console.error(
      `\nYou can install manually:\n  cargo install --git https://github.com/${REPO}`
    );
    // Don't exit 1 — allow npm install to succeed even if binary download fails
    // (user may want to build from source or run in an unsupported env).
  });

function downloadAndExtract(url, archiveName, binaryName, destPath) {
  return new Promise((resolve, reject) => {
    const tmpFile = path.join(os.tmpdir(), archiveName);
    const stream = fs.createWriteStream(tmpFile);

    followRedirects(url, (res) => {
      if (res.statusCode !== 200) {
        reject(
          new Error(`HTTP ${res.statusCode} downloading ${url}`)
        );
        return;
      }
      res.pipe(stream);
      stream.on("finish", () => {
        stream.close(() => {
          extract(tmpFile, archiveName, binaryName, destPath)
            .then(resolve)
            .catch(reject);
        });
      });
      stream.on("error", reject);
    });
  });
}

function followRedirects(url, callback) {
  https.get(url, (res) => {
    if (res.statusCode === 301 || res.statusCode === 302 || res.statusCode === 307 || res.statusCode === 308) {
      followRedirects(res.headers.location, callback);
    } else {
      callback(res);
    }
  }).on("error", (err) => {
    throw err;
  });
}

function extract(archivePath, archiveName, binaryName, destPath) {
  return new Promise((resolve, reject) => {
    try {
      if (archiveName.endsWith(".tar.gz")) {
        execSync(`tar -xzf "${archivePath}" -C "${path.dirname(destPath)}" "${binaryName}"`, {
          stdio: "pipe",
        });
      } else if (archiveName.endsWith(".zip")) {
        execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${path.dirname(destPath)}' -Force"`, {
          stdio: "pipe",
        });
      }
      resolve();
    } catch (err) {
      reject(new Error(`Extraction failed: ${err.message}`));
    }
  });
}
