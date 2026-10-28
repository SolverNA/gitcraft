'use strict';
// WebCrypto whole-file digests are bounded to 64 MiB/file; larger files use streaming Rust CLI.
const $ = id => document.getElementById(id);
const limit = 64 * 1024 * 1024;
let session = JSON.parse(localStorage.getItem('gitcraft.transfer') || 'null');
const say = value => { $('output').textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2); };
const digest = async bytes => [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))].map(b => b.toString(16).padStart(2, '0')).join('');
async function api(path, method = 'GET', body = null, headers = {}) {
  const response = await fetch(path, {method, credentials: 'same-origin', headers: {...headers}, body});
  const value = await response.json();
  if (!response.ok) throw new Error(value.error || `HTTP ${response.status}`);
  return value;
}
function auth() { if (!session) throw new Error('Сначала создайте перенос.'); return {Authorization: `Bearer ${session.token}`}; }
function route(suffix = '') { if (!session) throw new Error('Сессия переноса отсутствует.'); return `/api/transfers/${session.id}${suffix}`; }
async function showStatus() { const value = await api(route(), 'GET', null, auth()); say(value); return value; }
async function upload() {
  if (!$('consistent').checked) throw new Error('Сначала остановите локальный Minecraft или подготовьте согласованный snapshot, затем подтвердите источник.');
  const kind = $('kind').value;
  const files = [...$('files').files, ...$('folder').files];
  if (!files.length) throw new Error('Выберите файлы или папку.');
  // Reject every oversized file before reading any contents. Never silently skip.
  for (const f of files) if (f.size > limit) throw new Error(`${f.name}: ${f.size} bytes > 64 MiB. Используйте gitcraft transfer plan/send; точные команды приведены над выбором файлов.`);
  const manifest = {source_consistency: $('consistency').value, files: []};
  let total = 0;
  for (let i = 0; i < files.length; i++) {
    const f = files[i];
    const path = kind === 'world' ? f.webkitRelativePath : f.name;
    if (kind === 'world' && !path) throw new Error('Для мира выберите папку целиком: путь должен содержать имя корневого каталога мира.');
    say(`SHA-256 ${i + 1}/${files.length}: ${path}`);
    manifest.files.push({kind, path, size: f.size, sha256: await digest(await f.arrayBuffer())});
    total += f.size;
  }
  if (session && JSON.stringify(session.manifest) !== JSON.stringify(manifest)) {
    throw new Error('Выбранный источник отличается от сохранённой сессии. Для нового переноса удалите session через кнопку «Новая сессия»; старый staged transfer сохранится на сервере.');
  }
  if (!session) {
    const created = await api('/api/transfers', 'POST', JSON.stringify(manifest), {'Content-Type': 'application/json', 'X-CSRF-Token': $('csrf').value});
    session = {...created, manifest};
    localStorage.setItem('gitcraft.transfer', JSON.stringify(session));
  }
  const current = await api(route(), 'GET', null, auth());
  if (JSON.stringify(current.manifest) !== JSON.stringify(manifest)) {
    // Rust JSON key order differs; compare each typed field explicitly.
    if (current.manifest.source_consistency !== manifest.source_consistency || current.manifest.files.length !== manifest.files.length || current.manifest.files.some((f, i) => ['kind', 'path', 'size', 'sha256'].some(k => f[k] !== manifest.files[i][k]))) throw new Error('Manifest сервера отличается от локального.');
  }
  let sent = current.offsets.reduce((a, b) => a + b, 0);
  for (let i = 0; i < files.length; i++) {
    const f = files[i];
    for (let offset = current.offsets[i]; offset < f.size;) {
      const bytes = await f.slice(offset, offset + current.chunk_bytes).arrayBuffer();
      const response = await api(route(`/files/${i}/${offset}`), 'PUT', bytes, {...auth(), 'X-Chunk-SHA256': await digest(bytes), 'Content-Type': 'application/octet-stream'});
      const accepted = response.offset - offset; offset = response.offset; sent += accepted;
      $('progress').value = total ? sent * 100 / total : 100;
      say(`${f.name}: ${offset}/${f.size}; всего ${sent}/${total} bytes. Сессия ${session.id}`);
    }
  }
  say(await api(route('/verify'), 'POST', '{}', {...auth(), 'Content-Type': 'application/json'}));
}
function run(fn) { return async () => { try { await fn(); } catch (error) { say(`Ошибка: ${error.message}\nПеренос не считается завершённым. Повторите с теми же файлами для resume.`); } }; }
$('send').onclick = run(upload);
$('status').onclick = run(showStatus);
$('verify').onclick = run(async () => say(await api(route('/verify'), 'POST', '{}', {...auth(), 'Content-Type': 'application/json'})));
$('commit').onclick = run(async () => {
  if (!$('confirm').checked) throw new Error('Подтвердите остановку VPS Minecraft и замену миров с backup.');
  say(await api(route('/commit'), 'POST', JSON.stringify({confirm_world_replace: true}), {...auth(), 'Content-Type': 'application/json'}));
});
$('recover').onclick = run(async () => say(await api(route('/recover'), 'POST', '{}', {...auth(), 'Content-Type': 'application/json'})));
const fresh = document.createElement('button'); fresh.textContent = 'Новая сессия'; fresh.onclick = () => { session = null; localStorage.removeItem('gitcraft.transfer'); say('Выберите новый источник. Предыдущие staged-файлы на сервере не удалены.'); }; $('output').after(fresh);
if (session) say(`Есть сохранённая сессия ${session.id}. Для resume выберите те же файлы/папку; «Статус» доступен без выбора.`);

// Lifecycle administration uses the authenticated browser session + CSRF, never
// a raw admin token in JavaScript. A scoped transfer token can only cancel itself.
let cleanup = null;
const adminHeaders = () => ({'Content-Type': 'application/json', 'X-CSRF-Token': $('csrf').value});
const lifecycleSay = value => { $('sessionlist').textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2); };
function lifecycleRun(fn) { return async () => { try { await fn(); } catch (error) { lifecycleSay(`Ошибка: ${error.message}`); } }; }
if ($('sessions')) {
  $('sessions').onclick = lifecycleRun(async () => lifecycleSay(await api('/api/transfer-sessions', 'GET', null, adminHeaders())));
  $('cleanupplan').onclick = lifecycleRun(async () => {
    const ids = $('cleanupids').value.trim().split(/\s+/).filter(Boolean);
    cleanup = await api('/api/transfer-sessions/cleanup-plan', 'POST', JSON.stringify({ids}), adminHeaders());
    $('cleanupreviewed').checked = false;
    lifecycleSay(cleanup);
  });
  $('cleanupapply').onclick = lifecycleRun(async () => {
    if (!cleanup || !$('cleanupreviewed').checked) throw new Error('Сначала получите план очистки и подтвердите его проверку.');
    const result = await api('/api/transfer-sessions/cleanup-apply', 'POST', JSON.stringify({digest: cleanup.digest, reviewed: true}), adminHeaders());
    lifecycleSay(result); cleanup = null; $('cleanupreviewed').checked = false;
    if (session && result.removed.includes(session.id)) { session = null; localStorage.removeItem('gitcraft.transfer'); say('Staging сессии удалён; импортированный cache и receipts сохранены.'); }
  });
  $('cancel').onclick = lifecycleRun(async () => { lifecycleSay(await api(route('/cancel'), 'POST', '{}', {...auth(), 'Content-Type': 'application/json'})); });
  const cancelById = document.createElement('button'); cancelById.textContent = 'Отменить выбранную сессию по ID';
  cancelById.onclick = lifecycleRun(async () => {
    const ids = $('cleanupids').value.trim().split(/\s+/).filter(Boolean);
    if (ids.length !== 1) throw new Error('Укажите ровно один ID в поле выбора сессий.');
    lifecycleSay(await api('/api/transfer-sessions/cancel', 'POST', JSON.stringify({id: ids[0]}), adminHeaders()));
  });
  $('cancel').after(cancelById);
}
