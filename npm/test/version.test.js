'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { workspaceVersion, checkVersion } = require('../scripts/version');

const CARGO = fs.readFileSync(path.join(__dirname, '..', '..', 'Cargo.toml'), 'utf8');

test('reads the workspace package version from the real Cargo.toml', () => {
  assert.match(workspaceVersion(CARGO), /^\d+\.\d+\.\d+$/);
});

test('ignores version keys outside [workspace.package]', () => {
  const toml = '[workspace.dependencies]\nfoo = { version = "9.9.9" }\n[workspace.package]\nedition = "2021"\nversion = "1.2.3"\n[other]\nversion = "7.7.7"\n';
  assert.equal(workspaceVersion(toml), '1.2.3');
});

test('throws when there is no workspace version', () => {
  assert.throws(() => workspaceVersion('[package]\nversion = "1.0.0"\n'), /workspace/i);
});

test('checkVersion accepts the matching version', () => {
  assert.equal(checkVersion('1.2.3', '[workspace.package]\nversion = "1.2.3"\n'), '1.2.3');
});

test('checkVersion rejects a mismatch and names both versions', () => {
  assert.throws(() => checkVersion('1.2.4', '[workspace.package]\nversion = "1.2.3"\n'), /1\.2\.4.*1\.2\.3|1\.2\.3.*1\.2\.4/);
});

test('checkVersion rejects a tag-shaped or malformed version', () => {
  for (const v of ['v1.2.3', '1.2', '', 'latest']) {
    assert.throws(() => checkVersion(v, '[workspace.package]\nversion = "1.2.3"\n'), /version/i, v);
  }
});

const { spawnSync } = require('node:child_process');
const CLI = path.join(__dirname, '..', 'scripts', 'version.js');

test('CLI without arguments prints the workspace version', () => {
  const r = spawnSync(process.execPath, [CLI], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout.trim(), workspaceVersion(CARGO));
});

test('CLI with the matching version succeeds, with a mismatch fails', () => {
  const ok = spawnSync(process.execPath, [CLI, workspaceVersion(CARGO)], { encoding: 'utf8' });
  assert.equal(ok.status, 0, ok.stderr);
  const bad = spawnSync(process.execPath, [CLI, '99.0.0'], { encoding: 'utf8' });
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /mismatch/);
});
