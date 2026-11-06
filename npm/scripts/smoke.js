'use strict';

// Packs the staged host-platform package and the launcher, installs them into a
// clean project, and speaks MCP to the installed `konnect` command.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync, spawn } = require('node:child_process');
const { platformKey, packageFor } = require('../konnect/lib/platform');

const HANDSHAKE_TIMEOUT_MS = 20000;

function pack(dir, dest) {
  const out = execFileSync('npm', ['pack', '--silent', '--pack-destination', dest], { cwd: dir, encoding: 'utf8', shell: process.platform === 'win32' });
  return path.join(dest, out.trim().split('\n').pop());
}

function handshake(bin) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, [], { stdio: ['pipe', 'pipe', 'inherit'], shell: process.platform === 'win32' });
    const timer = setTimeout(() => { child.kill(); reject(new Error('no initialize response within timeout')); }, HANDSHAKE_TIMEOUT_MS);
    let buf = '';
    child.stdout.on('data', (chunk) => {
      buf += chunk;
      const nl = buf.indexOf('\n');
      if (nl < 0) return;
      clearTimeout(timer);
      child.stdin.end();
      try {
        resolve(JSON.parse(buf.slice(0, nl)));
      } catch (e) {
        reject(new Error(`first stdout line is not JSON (stdout must carry only protocol): ${buf.slice(0, nl)}`));
      }
    });
    child.on('error', reject);
    child.stdin.write(`${JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'npm-smoke', version: '0' } },
    })}\n`);
  });
}

async function smoke({ version, npmDir }) {
  const key = platformKey(process.platform, process.arch);
  if (!key) throw new Error(`smoke cannot run on ${process.platform}-${process.arch}`);
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'konnect-smoke-'));
  const tarballs = [pack(path.join(npmDir, `konnect-${key}`), work), pack(path.join(npmDir, 'konnect'), work)];
  const project = path.join(work, 'project');
  fs.mkdirSync(project);
  fs.writeFileSync(path.join(project, 'package.json'), '{"private":true}\n');
  execFileSync('npm', ['install', '--no-audit', '--no-fund', '--omit=optional', ...tarballs], { cwd: project, stdio: 'inherit', shell: process.platform === 'win32' });

  const bin = path.join(project, 'node_modules', '.bin', process.platform === 'win32' ? 'konnect.cmd' : 'konnect');
  const reported = execFileSync(bin, ['--version'], { encoding: 'utf8', shell: process.platform === 'win32' }).trim();
  if (reported !== `konnect ${version}`) throw new Error(`binary reports "${reported}", expected "konnect ${version}"`);

  const reply = await handshake(bin);
  if (!reply.result || !reply.result.serverInfo) throw new Error(`unexpected initialize reply: ${JSON.stringify(reply)}`);
  console.log(`ok: ${packageFor(key)} ${version} answered initialize as ${reply.result.serverInfo.name}`);
}

module.exports = { smoke };

if (require.main === module) {
  const [version, npmDir] = process.argv.slice(2);
  smoke({ version, npmDir: path.resolve(npmDir || path.join(__dirname, '..')) }).catch((e) => {
    console.error(`smoke failed: ${e.message}`);
    process.exit(1);
  });
}
