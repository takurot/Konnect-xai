'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { PLATFORM_KEYS, packageFor, binaryFile } = require('../konnect/lib/platform');
const { assertVersion } = require('./version');

const TARGETS = Object.freeze({
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'darwin-x64': 'x86_64-apple-darwin',
  'darwin-arm64': 'aarch64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
});

const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));
const writeJson = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);

function findArchive(artifactsDir, target) {
  const names = fs.readdirSync(artifactsDir).filter((n) =>
    new RegExp(`^konnect-.+-${target}\\.(tar\\.gz|zip)$`).test(n));
  if (names.length !== 1) {
    throw new Error(`expected exactly one release archive for ${target} in ${artifactsDir}, found ${names.length}`);
  }
  return path.join(artifactsDir, names[0]);
}

function extract(archive, binary, destDir) {
  fs.mkdirSync(destDir, { recursive: true });
  if (archive.endsWith('.zip')) {
    execFileSync('unzip', ['-o', '-q', archive, binary, '-d', destDir]);
  } else {
    execFileSync('tar', ['xzf', archive, '-C', destDir, binary]);
  }
  const out = path.join(destDir, binary);
  if (!fs.existsSync(out)) throw new Error(`${archive} does not contain ${binary}`);
  if (!binary.endsWith('.exe')) fs.chmodSync(out, 0o755);
}

function stage({ version, artifactsDir, npmDir }) {
  assertVersion(version);

  const archives = PLATFORM_KEYS.map((key) => ({
    key,
    archive: findArchive(artifactsDir, TARGETS[key]),
  }));

  for (const { key, archive } of archives) {
    const dir = path.join(npmDir, `konnect-${key}`);
    extract(archive, binaryFile(key.split('-')[0]), path.join(dir, 'bin'));
    const file = path.join(dir, 'package.json');
    writeJson(file, { ...readJson(file), version });
  }

  const mainFile = path.join(npmDir, 'konnect', 'package.json');
  const main = readJson(mainFile);
  writeJson(mainFile, {
    ...main,
    version,
    optionalDependencies: Object.fromEntries(PLATFORM_KEYS.map((k) => [packageFor(k), version])),
  });
}

module.exports = { stage, TARGETS };

if (require.main === module) {
  const [version, artifactsDir] = process.argv.slice(2);
  try {
    if (!version || !artifactsDir) throw new Error('usage: stage.js <version> <artifacts-dir>');
    stage({ version, artifactsDir: path.resolve(artifactsDir), npmDir: path.join(__dirname, '..') });
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
}
