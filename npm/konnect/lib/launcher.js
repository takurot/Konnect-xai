'use strict';

const fs = require('node:fs');
const { spawn } = require('node:child_process');
const { resolveBinary, UnsupportedPlatformError, MissingBinaryError } = require('./platform');

const FORWARDED_SIGNALS = ['SIGINT', 'SIGTERM', 'SIGHUP'];
const LINUX_REQUIREMENTS = 'konnect: this Linux package requires glibc 2.35 or newer (musl is unsupported).\n';

function ensureExecutable(file) {
  if (process.platform === 'win32') return;
  try {
    fs.accessSync(file, fs.constants.X_OK);
  } catch {
    fs.chmodSync(file, 0o755);
  }
}

function main(argv) {
  let binary;
  try {
    binary = resolveBinary({
      platform: process.platform,
      arch: process.arch,
      resolve: (id) => require.resolve(id),
    });
    ensureExecutable(binary);
  } catch (e) {
    if (e instanceof UnsupportedPlatformError || e instanceof MissingBinaryError) {
      process.stderr.write(`konnect: ${e.message}\n`);
      if (process.platform === 'linux' && e instanceof MissingBinaryError) {
        process.stderr.write(LINUX_REQUIREMENTS);
      }
    } else {
      process.stderr.write(`konnect: cannot prepare the server binary: ${e && e.message}\n`);
    }
    process.exit(1);
  }

  const child = spawn(binary, argv, { stdio: 'inherit', windowsHide: true });

  for (const sig of FORWARDED_SIGNALS) {
    process.on(sig, () => child.kill(sig));
  }

  child.on('error', (e) => {
    process.stderr.write(`konnect: failed to start ${binary}: ${e.message}\n`);
    if (process.platform === 'linux' && e.code === 'ENOENT') {
      process.stderr.write(LINUX_REQUIREMENTS);
    }
    process.exit(1);
  });

  child.on('exit', (code, signal) => {
    if (signal) {
      for (const sig of FORWARDED_SIGNALS) process.removeAllListeners(sig);
      process.kill(process.pid, signal);
      return;
    }
    // A dynamic-loader failure exits with 127 after writing its own diagnostic.
    if (process.platform === 'linux' && code === 127) {
      process.stderr.write(LINUX_REQUIREMENTS);
    }
    process.exit(code === null ? 1 : code);
  });
}

module.exports = { main };
