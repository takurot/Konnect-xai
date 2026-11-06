'use strict';

const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;

function workspaceVersion(cargoToml) {
  let inSection = false;
  for (const raw of cargoToml.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('[')) {
      inSection = line === '[workspace.package]';
      continue;
    }
    const m = inSection && /^version\s*=\s*"([^"]+)"/.exec(line);
    if (m) return m[1];
  }
  throw new Error('no version found under [workspace.package] in Cargo.toml');
}

function assertVersion(version) {
  if (typeof version !== 'string' || !SEMVER.test(version)) {
    throw new Error(`invalid version "${version}": expected MAJOR.MINOR.PATCH without a leading "v"`);
  }
}

function checkVersion(version, cargoToml) {
  assertVersion(version);
  const cargo = workspaceVersion(cargoToml);
  if (cargo !== version) {
    throw new Error(`version mismatch: release is ${version} but Cargo.toml workspace version is ${cargo}`);
  }
  return version;
}

module.exports = { assertVersion, workspaceVersion, checkVersion };

if (require.main === module) {
  const fs = require('node:fs');
  const path = require('node:path');
  try {
    const cargo = fs.readFileSync(path.join(__dirname, '..', '..', 'Cargo.toml'), 'utf8');
    const wanted = process.argv[2];
    console.log(wanted === undefined ? workspaceVersion(cargo) : checkVersion(wanted, cargo));
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
}
