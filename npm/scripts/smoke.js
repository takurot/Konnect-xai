'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const readline = require('node:readline');
const { execFileSync, spawn } = require('node:child_process');
const { platformKey, packageFor, binaryFile } = require('../konnect/lib/platform');
const { assertVersion } = require('./version');

const HANDSHAKE_TIMEOUT_MS = 60000;

function npm(args, options) {
  if (process.platform === 'win32') {
    const cli = path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js');
    return execFileSync(process.execPath, [cli, ...args], { timeout: 180000, ...options });
  }
  return execFileSync('npm', args, { timeout: 180000, ...options });
}

function pack(dir, dest, env) {
  const out = npm(['pack', '--ignore-scripts', '--silent', '--pack-destination', dest], { cwd: dir, env, encoding: 'utf8' });
  return path.join(dest, out.trim().split('\n').pop());
}

function handshake(command, args = [], { cwd, env } = {}) {
  const requests = [
    ['initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'npm-smoke', version: '0' } }],
    ['tools/list', {}],
    ['tools/call', { name: 'get_installation_info', arguments: {} }],
  ];
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['pipe', 'pipe', 'inherit'] });
    const lines = readline.createInterface({ input: child.stdout });
    const results = [];
    let failure;
    const fail = (error) => {
      if (failure) return;
      failure = error;
      lines.close();
      child.stdin.destroy();
      child.stdout.destroy();
      child.kill();
    };
    const timer = setTimeout(() => fail(new Error('MCP smoke deadline exceeded')), HANDSHAKE_TIMEOUT_MS);
    const send = (message) => child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', ...message })}\n`);
    const request = () => {
      const [method, params] = requests[results.length];
      send({ id: results.length + 1, method, params });
    };
    child.on('error', fail);
    child.stdin.on('error', fail);
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      lines.close();
      if (failure) {
        reject(failure);
      } else if (code !== 0 || results.length !== requests.length) {
        reject(new Error(`launcher closed before successful MCP smoke (${signal ?? code})`));
      } else {
        resolve(results);
      }
    });
    lines.on('line', (line) => {
      if (failure) return;
      const method = requests[results.length]?.[0] ?? 'after smoke';
      try {
        const { jsonrpc, id, error, result } = JSON.parse(line);
        if (jsonrpc !== '2.0' || id !== results.length + 1 || results.length === requests.length) {
          throw new Error(`unexpected response envelope for ${method}`);
        }
        if (error || !result || typeof result !== 'object' || Array.isArray(result)) {
          throw new Error(`${method}: ${error?.message ?? 'missing result object'}`);
        }
        results.push(result);
        if (results.length === 1) send({ method: 'notifications/initialized', params: {} });
        if (results.length === requests.length) child.stdin.end();
        else request();
      } catch (error) {
        fail(error);
      }
    });
    request();
  });
}

async function smoke({ version, npmDir }) {
  assertVersion(version);
  const key = platformKey(process.platform, process.arch);
  if (!key) throw new Error(`smoke cannot run on ${process.platform}-${process.arch}`);

  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'konnect-smoke-'));
  try {
    const project = path.join(work, 'project');
    fs.mkdirSync(project);
    fs.writeFileSync(path.join(project, 'package.json'), '{"private":true}\n');
    const npmrc = path.join(work, 'npmrc');
    fs.writeFileSync(npmrc, '');
    const env = {
      ...process.env,
      KONNECT_STATE_DIR: path.join(work, 'state'),
      npm_config_cache: path.join(work, 'npm-cache'),
      npm_config_userconfig: npmrc,
      npm_config_registry: 'https://registry.npmjs.org',
    };
    const packages = npmDir ? [
      '--omit=optional',
      pack(path.join(npmDir, `konnect-${key}`), work, env),
      pack(path.join(npmDir, 'konnect'), work, env),
    ] : ['--include=optional', `@konnect-xai/konnect@${version}`];
    npm(['install', '--no-audit', '--no-fund', '--ignore-scripts', ...packages], { cwd: project, env, stdio: 'inherit' });

    const modules = path.join(project, 'node_modules');
    const launcher = path.join(modules, '@konnect-xai', 'konnect', 'bin', 'konnect.js');
    const expected = path.join(modules, packageFor(key), 'bin', binaryFile(process.platform));
    const reported = execFileSync(process.execPath, [launcher, '--version'], { cwd: project, env, encoding: 'utf8', timeout: 60000 }).trim();
    if (reported !== `konnect ${version}`) throw new Error(`binary reports "${reported}", expected "konnect ${version}"`);
    const [initialized, listed, installation] = await handshake(process.execPath, [launcher], { cwd: project, env });
    if (initialized.serverInfo?.name !== 'konnect' || initialized.serverInfo.version !== version) {
      throw new Error('initialize server name or version did not match the package');
    }
    if (!Array.isArray(listed.tools) || !listed.tools.some((tool) => tool.name === 'get_installation_info')) {
      throw new Error('tools/list omitted get_installation_info');
    }
    const info = JSON.parse(installation.content?.[0]?.text ?? '{}');
    if (installation.isError || info.build?.version !== version || typeof info.runtime?.executable_path !== 'string'
        || fs.realpathSync(info.runtime.executable_path) !== fs.realpathSync(expected)) {
      throw new Error('installation info did not identify the expected package binary and version');
    }
    console.log(`ok: ${packageFor(key)} ${version} answered initialize, tools/list and get_installation_info`);
  } finally {
    fs.rmSync(work, { recursive: true, force: true, maxRetries: 5 });
  }
}

module.exports = { handshake, smoke };

if (require.main === module) {
  const [version, source] = process.argv.slice(2);
  const npmDir = source === '--published' ? undefined : path.resolve(source || path.join(__dirname, '..'));
  smoke({ version, npmDir }).catch((error) => {
    console.error(`smoke failed: ${error.message}`);
    process.exitCode = 1;
  });
}
