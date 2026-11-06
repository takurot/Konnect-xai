'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync, spawn } = require('node:child_process');

const LAUNCHER = path.join(__dirname, '..', 'konnect', 'bin', 'konnect.js');
const posix = process.platform !== 'win32';

function installLauncher(root) {
  fs.mkdirSync(path.join(root, 'bin'), { recursive: true });
  fs.copyFileSync(LAUNCHER, path.join(root, 'bin', 'konnect.js'));
  fs.cpSync(path.join(__dirname, '..', 'konnect', 'lib'), path.join(root, 'lib'), { recursive: true });
}

// Host's own platform key, with a stub platform package carrying a node script
// in place of the real binary.
function stubInstall(script, { mode = 0o755 } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'konnect-npm-'));
  const key = `${process.platform}-${process.arch}`;
  const bin = path.join(root, 'node_modules', '@konnect-xai', `konnect-${key}`, 'bin');
  fs.mkdirSync(bin, { recursive: true });
  const file = path.join(bin, process.platform === 'win32' ? 'konnect.exe' : 'konnect');
  fs.writeFileSync(file, `#!/usr/bin/env node\n${script}\n`, { mode });
  installLauncher(root);
  return { root, file };
}

const run = (root, args, opts = {}) =>
  spawnSync(process.execPath, [path.join(root, 'bin', 'konnect.js'), ...args], { encoding: 'utf8', ...opts });

test('forwards arguments and stdout, stdin reaches the server', { skip: !posix }, () => {
  const { root } = stubInstall(
    `let d='';process.stdin.on('data',c=>d+=c).on('end',()=>process.stdout.write(JSON.stringify({args:process.argv.slice(2),stdin:d})));`,
  );
  const r = run(root, ['--config', '/c.json'], { input: 'hello' });
  assert.equal(r.status, 0);
  assert.deepEqual(JSON.parse(r.stdout), { args: ['--config', '/c.json'], stdin: 'hello' });
});

test('propagates the exit code', { skip: !posix }, () => {
  const { root } = stubInstall('process.exit(7)');
  assert.equal(run(root, []).status, 7);
});

test('stdout carries only what the server wrote', { skip: !posix }, () => {
  const { root } = stubInstall(`process.stdout.write('{"ok":1}\\n');process.stderr.write('log\\n');`);
  const r = run(root, []);
  assert.equal(r.stdout, '{"ok":1}\n');
  assert.equal(r.stderr, 'log\n');
});

test('a terminating signal reaches the server and is reflected in the exit', { skip: !posix, timeout: 10000 }, async () => {
  const { root } = stubInstall(`process.on('SIGTERM',()=>process.exit(42));process.stdout.write('ready\\n');setTimeout(()=>process.exit(0),5000);`);
  const child = spawn(process.execPath, [path.join(root, 'bin', 'konnect.js')], { stdio: ['ignore', 'pipe', 'pipe'] });
  await new Promise((res) => child.stdout.once('data', res));
  child.kill('SIGTERM');
  const code = await new Promise((res) => child.on('exit', (c) => res(c)));
  assert.equal(code, 42);
});

test('a non-executable binary is repaired instead of failing with EACCES', { skip: !posix }, () => {
  const { root } = stubInstall(`process.stdout.write('ran');`, { mode: 0o644 });
  const r = run(root, []);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout, 'ran');
});

test('missing platform package: message on stderr, nothing on stdout, non-zero exit', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'konnect-npm-empty-'));
  installLauncher(root);
  const r = run(root, []);
  assert.notEqual(r.status, 0);
  assert.equal(r.stdout, '');
  assert.match(r.stderr, /@konnect-xai\/konnect-/);
});
