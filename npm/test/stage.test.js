'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync, spawnSync } = require('node:child_process');
const { PLATFORM_KEYS, packageFor } = require('../konnect/lib/platform');
const { stage, TARGETS } = require('../scripts/stage');

const NPM_DIR = path.join(__dirname, '..');
const hasZip = spawnSync('zip', ['-v']).status === 0 && spawnSync('unzip', ['-v']).status === 0;
const tmp = (p) => fs.mkdtempSync(path.join(os.tmpdir(), p));
const readJson = (f) => JSON.parse(fs.readFileSync(f, 'utf8'));

function copyTemplate() {
  const dir = tmp('konnect-stage-npm-');
  for (const entry of fs.readdirSync(NPM_DIR)) {
    if (entry.startsWith('konnect')) fs.cpSync(path.join(NPM_DIR, entry), path.join(dir, entry), { recursive: true });
  }
  return dir;
}

function makeArtifacts({ skip } = {}) {
  const dir = tmp('konnect-stage-art-');
  for (const key of PLATFORM_KEYS) {
    const target = TARGETS[key];
    if (target === skip) continue;
    const work = tmp('konnect-stage-bin-');
    const win = key.startsWith('win32');
    const exe = win ? 'konnect.exe' : 'konnect';
    fs.writeFileSync(path.join(work, exe), `binary-for-${target}`);
    if (win) execFileSync('zip', ['-q', path.join(dir, `konnect-v9.9.9-${target}.zip`), exe], { cwd: work });
    else execFileSync('tar', ['czf', path.join(dir, `konnect-v9.9.9-${target}.tar.gz`), exe], { cwd: work });
  }
  return dir;
}

test('template: platform packages match the key table', () => {
  for (const key of PLATFORM_KEYS) {
    const pkg = readJson(path.join(NPM_DIR, `konnect-${key}`, 'package.json'));
    const [o, c] = key.split('-');
    assert.equal(pkg.name, packageFor(key));
    assert.deepEqual(pkg.os, [o]);
    assert.deepEqual(pkg.cpu, [c]);
    assert.deepEqual(pkg.files, ['bin']);
  }
});

test('template: main package wires the launcher and every platform package', () => {
  const pkg = readJson(path.join(NPM_DIR, 'konnect', 'package.json'));
  assert.equal(pkg.name, '@konnect-xai/konnect');
  assert.equal(pkg.bin.konnect, 'bin/konnect.js');
  assert.deepEqual(Object.keys(pkg.optionalDependencies).sort(), PLATFORM_KEYS.map(packageFor).sort());
  assert.ok(pkg.files.includes('bin') && pkg.files.includes('lib'));
  assert.ok(pkg.engines.node);
});

test('stage sets every version, pins optionalDependencies, and places the binaries', { skip: !hasZip }, () => {
  const npmDir = copyTemplate();
  stage({ version: '1.2.3', artifactsDir: makeArtifacts(), npmDir });
  for (const key of PLATFORM_KEYS) {
    const dir = path.join(npmDir, `konnect-${key}`);
    assert.equal(readJson(path.join(dir, 'package.json')).version, '1.2.3');
    const file = path.join(dir, 'bin', key.startsWith('win32') ? 'konnect.exe' : 'konnect');
    assert.equal(fs.readFileSync(file, 'utf8'), `binary-for-${TARGETS[key]}`);
    if (!key.startsWith('win32')) assert.ok(fs.statSync(file).mode & 0o100, 'owner-executable');
  }
  const main = readJson(path.join(npmDir, 'konnect', 'package.json'));
  assert.equal(main.version, '1.2.3');
  for (const key of PLATFORM_KEYS) assert.equal(main.optionalDependencies[packageFor(key)], '1.2.3');
});

test('stage refuses before writing anything when one archive is missing', { skip: !hasZip }, () => {
  const npmDir = copyTemplate();
  const before = fs.readFileSync(path.join(npmDir, 'konnect', 'package.json'), 'utf8');
  assert.throws(
    () => stage({ version: '1.2.3', artifactsDir: makeArtifacts({ skip: TARGETS['linux-arm64'] }), npmDir }),
    /aarch64-unknown-linux-gnu/,
  );
  assert.equal(fs.readFileSync(path.join(npmDir, 'konnect', 'package.json'), 'utf8'), before);
  assert.ok(!fs.existsSync(path.join(npmDir, 'konnect-linux-x64', 'bin')));
});

test('stage is idempotent', { skip: !hasZip }, () => {
  const npmDir = copyTemplate();
  const art = makeArtifacts();
  stage({ version: '1.2.3', artifactsDir: art, npmDir });
  stage({ version: '1.2.3', artifactsDir: art, npmDir });
  assert.equal(readJson(path.join(npmDir, 'konnect', 'package.json')).version, '1.2.3');
});

test('stage rejects an invalid version', () => {
  assert.throws(() => stage({ version: 'v1.2.3', artifactsDir: '/nonexistent', npmDir: '/nonexistent' }), /version/i);
});
