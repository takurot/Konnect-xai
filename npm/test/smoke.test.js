'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { handshake } = require('../scripts/smoke');

const expected = { version: '1.2.3' };

function fakeServer(breakList, protocolVersion = '2025-06-18') {
  return [
    '-e',
    `
    const readline = require('node:readline');
    readline.createInterface({input: process.stdin}).on('line', (line) => {
      const { id, method } = JSON.parse(line);
      const reply = (result) => console.log(JSON.stringify({jsonrpc:'2.0', id, result}));
      if (method === 'initialize') reply({protocolVersion:${JSON.stringify(protocolVersion)},serverInfo:{name:'konnect',version:'1.2.3'}});
      if (method === 'tools/list') {
        if (${breakList}) console.log(JSON.stringify({jsonrpc:'2.0', id, error:{code:-32603,message:'list failed'}}));
        else reply({tools:[{name:'get_installation_info'}]});
      }
      if (method === 'tools/call') reply({content:[{type:'text',text:JSON.stringify({
        build: {version:'1.2.3'},
        runtime: {executable_path:'/fixture/konnect'},
      })}]});
    });
    `,
  ];
}

test('launcher smoke refuses an initialize-only server whose tools/list fails', async () => {
  await assert.rejects(handshake(process.execPath, fakeServer(true), expected), /tools\/list.*list failed/);
});

test('launcher smoke returns all three correlated results', async () => {
  const [initialized, listed, installation] = await handshake(process.execPath, fakeServer(false), expected);
  assert.equal(initialized.serverInfo.version, '1.2.3');
  assert.deepEqual(listed.tools.map((tool) => tool.name), ['get_installation_info']);
  assert.equal(JSON.parse(installation.content[0].text).runtime.executable_path, '/fixture/konnect');
});

test('launcher smoke rejects a mismatched protocol during initialization', async () => {
  await assert.rejects(handshake(process.execPath, fakeServer(false, '2024-11-05'), expected), /initialize did not negotiate/);
});
