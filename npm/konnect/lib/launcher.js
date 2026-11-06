'use strict';

const fs = require('node:fs');
const { spawn } = require('node:child_process');
const { resolveBinary, UnsupportedPlatformError, MissingBinaryError } = require('./platform');

const FORWARDED_SIGNALS = ['SIGINT', 'SIGTERM', 'SIGHUP'];

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
    process.exit(1);
  });

  child.on('exit', (code, signal) => {
    if (signal) {
      for (const sig of FORWARDED_SIGNALS) process.removeAllListeners(sig);
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code === null ? 1 : code);
  });
}

module.exports = { main };
