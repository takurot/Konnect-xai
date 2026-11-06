'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const {
  PLATFORM_KEYS,
  platformKey,
  packageFor,
  binaryFile,
  resolveBinary,
  UnsupportedPlatformError,
  MissingBinaryError,
} = require('../konnect/lib/platform');

test('platformKey maps the five supported os/cpu pairs', () => {
  assert.equal(platformKey('linux', 'x64'), 'linux-x64');
  assert.equal(platformKey('linux', 'arm64'), 'linux-arm64');
  assert.equal(platformKey('darwin', 'x64'), 'darwin-x64');
  assert.equal(platformKey('darwin', 'arm64'), 'darwin-arm64');
  assert.equal(platformKey('win32', 'x64'), 'win32-x64');
});

test('platformKey rejects everything else', () => {
  for (const [p, a] of [['win32', 'arm64'], ['linux', 'ia32'], ['freebsd', 'x64'], ['linux', 'riscv64']]) {
    assert.equal(platformKey(p, a), null, `${p}-${a}`);
  }
});

test('PLATFORM_KEYS is exactly the published platform packages', () => {
  assert.deepEqual([...PLATFORM_KEYS].sort(), [
    'darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64',
  ]);
});

test('packageFor and binaryFile', () => {
  assert.equal(packageFor('linux-arm64'), '@konnect-xai/konnect-linux-arm64');
  assert.equal(binaryFile('win32'), 'konnect.exe');
  assert.equal(binaryFile('linux'), 'konnect');
  assert.equal(binaryFile('darwin'), 'konnect');
});

test('resolveBinary asks for bin/<binary> inside the platform package', () => {
  const asked = [];
  const path = resolveBinary({
    platform: 'linux', arch: 'arm64',
    resolve: (id) => { asked.push(id); return '/x/' + id; },
  });
  assert.deepEqual(asked, ['@konnect-xai/konnect-linux-arm64/bin/konnect']);
  assert.equal(path, '/x/@konnect-xai/konnect-linux-arm64/bin/konnect');
});

test('resolveBinary uses konnect.exe on win32', () => {
  const asked = [];
  resolveBinary({ platform: 'win32', arch: 'x64', resolve: (id) => { asked.push(id); return id; } });
  assert.deepEqual(asked, ['@konnect-xai/konnect-win32-x64/bin/konnect.exe']);
});

test('unsupported platform lists every supported one', () => {
  assert.throws(
    () => resolveBinary({ platform: 'freebsd', arch: 'x64', resolve: () => { throw new Error('unreached'); } }),
    (e) => {
      assert.ok(e instanceof UnsupportedPlatformError);
      assert.match(e.message, /freebsd-x64/);
      for (const k of PLATFORM_KEYS) assert.ok(e.message.includes(k), k);
      return true;
    },
  );
});

test('missing platform package explains how to recover', () => {
  assert.throws(
    () => resolveBinary({
      platform: 'linux', arch: 'x64',
      resolve: () => { const e = new Error('nope'); e.code = 'MODULE_NOT_FOUND'; throw e; },
    }),
    (e) => {
      assert.ok(e instanceof MissingBinaryError);
      assert.match(e.message, /@konnect-xai\/konnect-linux-x64/);
      assert.match(e.message, /optional/i);
      assert.match(e.message, /--no-optional|omit=optional/);
      return true;
    },
  );
});

test('unexpected resolve errors are not disguised as a missing package', () => {
  const boom = new Error('boom');
  assert.throws(
    () => resolveBinary({ platform: 'linux', arch: 'x64', resolve: () => { throw boom; } }),
    (e) => e === boom,
  );
});
