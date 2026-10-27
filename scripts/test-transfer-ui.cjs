// Execute actual browser handlers against a minimal DOM and a protocol fixture.
// This checks JS behavior, not browser rendering or SSH/TLS infrastructure.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const {webcrypto, createHash} = require('node:crypto');
const path = require('node:path');
const script = fs.readFileSync(path.join(__dirname, '../src/transfer.js'), 'utf8');
function fixture(file, checked = true) {
  const elements = Object.fromEntries(['files','folder','consistent','kind','consistency','csrf','output','progress','send','status','verify','commit','confirm','recover'].map(id => [id, {files: [], value: '', checked: false, after() {}}]));
  elements.files.files = [file]; elements.kind.value = 'world'; elements.consistency.value = 'stopped'; elements.consistent.checked = checked; elements.csrf.value = 'test-csrf';
  const calls = []; const stored = new Map(); let manifest;
  const context = {document: {getElementById: id => elements[id], createElement: () => ({})}, localStorage: {getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value), removeItem: key => stored.delete(key)}, crypto: webcrypto, Uint8Array, Error, JSON,
    fetch: async (url, request) => {
      calls.push({url, request}); let value;
      if (url === '/api/transfers') { assert.equal(request.headers['X-CSRF-Token'], 'test-csrf'); manifest = JSON.parse(request.body); value = {id: 'session', token: 'scoped-token', chunk_bytes: 1048576}; }
      else { assert.equal(request.headers.Authorization, 'Bearer scoped-token');
        if (request.method === 'GET') value = {manifest, offsets: [0], chunk_bytes: 1048576};
        else if (request.method === 'PUT') { assert.equal(request.headers['X-Chunk-SHA256'], createHash('sha256').update(Buffer.from(request.body)).digest('hex')); value = {offset: request.body.byteLength}; }
        else if (url.endsWith('/verify')) value = {verified: true, status: 'verified'};
        else throw new Error(`Unexpected route: ${url}`);
      }
      return {ok: true, json: async () => value};
    }};
  vm.runInNewContext(script, context); return {elements, calls, stored};
}
(async () => {
  let reads = 0;
  const huge = {name: 'huge.mca', webkitRelativePath: 'world/region/huge.mca', size: 64 * 1024 * 1024 + 1, arrayBuffer: async () => { reads++; throw new Error('must not read'); }};
  const oversized = fixture(huge); await oversized.elements.send.onclick();
  assert.equal(reads, 0); assert.equal(oversized.calls.length, 0); assert.match(oversized.elements.output.textContent, /64 MiB/);
  const unconfirmed = fixture(huge, false); await unconfirmed.elements.send.onclick(); assert.equal(unconfirmed.calls.length, 0); assert.equal(reads, 0);
  const bytes = Uint8Array.from([104,101,108,108,111]);
  const file = {name: 'level.dat', webkitRelativePath: 'world/level.dat', size: bytes.length, arrayBuffer: async () => bytes.buffer, slice: (start, end) => ({arrayBuffer: async () => bytes.slice(start,end).buffer})};
  const upload = fixture(file); await upload.elements.send.onclick();
  assert.match(upload.elements.output.textContent, /verified/); assert.equal(upload.calls.length, 4);
  assert.equal(JSON.parse(upload.calls[0].request.body).files[0].sha256, createHash('sha256').update(bytes).digest('hex'));
  assert.equal(JSON.parse(upload.stored.get('gitcraft.transfer')).token, 'scoped-token');
  console.log('Transfer UI size/consistency gates, whole SHA-256, chunk SHA-256 and authenticated upload passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
