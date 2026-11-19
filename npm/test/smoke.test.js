'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { handshake } = require('../scripts/smoke');

function fakeServer(breakList) {
  return [
    '-e',
    `
    const readline = require('node:readline');
    readline.createInterface({input: process.stdin}).on('line', (line) => {
      const { id, method } = JSON.parse(line);
      const reply = (result) => console.log(JSON.stringify({jsonrpc:'2.0', id, result}));
      if (method === 'initialize') reply({serverInfo:{name:'konnect',version:'1.2.3'}});
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
  await assert.rejects(handshake(process.execPath, fakeServer(true)), /tools\/list.*list failed/);
});
