"use strict";

const https = require("https");
const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const os = require("os");

const PLATFORM_MAP = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-musl",
};

function getTarget() {
  const key = `${process.platform}-${process.arch}`;
  const target = PLATFORM_MAP[key];
  if (!target) {
    console.error(
      `Error: beachcomber does not provide a pre-built binary for ${process.platform} ${process.arch}.\n\n` +
        `Install from source:  cargo install beachcomber\n` +
        `Supported platforms:  macOS (arm64, x86_64), Linux (x86_64)\n` +
        `More info:            https://beachcomber.sh`
    );
    process.exit(1);
  }
  return target;
}

function download(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          download(res.headers.location).then(resolve, reject);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode}`));
          return;
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

async function main() {
  const target = getTarget();
  const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, "package.json"), "utf8"));
  const version = pkg.version;
  const url = `https://github.com/NavistAu/beachcomber/releases/download/v${version}/beachcomber-v${version}-${target}.tar.gz`;

  console.log(`Downloading comb v${version} for ${target}...`);

  let tarball;
  try {
    tarball = await download(url);
  } catch (err) {
    console.error(
      `Error: Failed to download comb binary from GitHub Releases.\n\n` +
        `URL: ${url}\n` +
        `${err.message}\n\n` +
        `Try installing manually:\n` +
        `  brew install navistau/tap/beachcomber\n` +
        `  cargo install beachcomber\n\n` +
        `More info: https://beachcomber.sh`
    );
    process.exit(1);
  }

  const binDir = path.join(__dirname, "bin");
  fs.mkdirSync(binDir, { recursive: true });

  const tarballPath = path.join(os.tmpdir(), `beachcomber-${version}.tar.gz`);
  fs.writeFileSync(tarballPath, tarball);

  try {
    execSync(`tar xzf "${tarballPath}" -C "${binDir}" comb`, { stdio: "pipe" });
  } catch (err) {
    console.error(`Error: Failed to extract comb binary from tarball.\n${err.message}`);
    process.exit(1);
  } finally {
    fs.unlinkSync(tarballPath);
  }

  fs.chmodSync(path.join(binDir, "comb"), 0o755);
  console.log(`Installed comb v${version} to ${path.join(binDir, "comb")}`);
}

main();
