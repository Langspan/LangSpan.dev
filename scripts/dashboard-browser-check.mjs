#!/usr/bin/env node
// Browser checks for `langspan dashboard`: the real binary, a real daemon,
// real hooks and a real headless browser that speaks the DevTools protocol,
// driven with Node's own WebSocket. No npm dependencies.
//
//   node scripts/dashboard-browser-check.mjs --binary target/debug/langspan \
//     --browser /path/to/browser [--out DIR] [--axe path/to/axe.min.js]
//   node scripts/dashboard-browser-check.mjs ... --state-dir DIR --screens-only
//
// Without --state-dir it builds its own coordinator in a temporary
// directory, with hostile task, path and message text, and checks the live
// page (polling, token, 403, 503, stale view, selection, focus and scroll
// across refreshes) and the --demo page at 390, 768, 1024, 1280 and 1600 px
// in the light and dark themes: no page errors, no request beyond the page's
// own origin, no page-level horizontal overflow, and accessibility rules
// (names, labels, headings, unique IDs, text contrast). With --axe it also
// runs axe-core's rules. With --state-dir it only reads that coordinator,
// through `langspan dashboard`, and saves screenshots; it never writes there.
// Screenshots never show the URL, so they never show the token.
// Requires Node 22 or later (global WebSocket).

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, arg, i, all) => {
  if (arg.startsWith('--')) pairs.push([arg.slice(2), all[i + 1] && !all[i + 1].startsWith('--') ? all[i + 1] : true]);
  return pairs;
}, []));
const binary = path.resolve(args.binary || 'target/debug/langspan');
const browserPath = args.browser || process.env.LANGSPAN_BROWSER;
const out = path.resolve(args.out || 'target/dashboard-browser');
const WIDTHS = [390, 768, 1024, 1280, 1600];
const THEMES = ['light', 'dark'];
if (!browserPath) fail('pass --browser PATH or set LANGSPAN_BROWSER to a browser executable with the DevTools protocol');
if (typeof WebSocket !== 'function') fail('Node 22 or later is required for its WebSocket');
fs.mkdirSync(out, { recursive: true });

const failures = [];
const children = [];
let base = null;
function fail(message) { console.error('dashboard-browser-check: ' + message); process.exit(2); }
function check(ok, what) {
  console.log((ok ? 'ok   ' : 'FAIL ') + what);
  if (!ok) failures.push(what);
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(what, probe, ms = 10000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await probe();
    if (value) return value;
    if (Date.now() > deadline) throw new Error('timed out waiting for ' + what);
    await sleep(100);
  }
}
function cleanup() {
  for (const child of children) { try { child.kill('SIGKILL'); } catch { /* gone */ } }
  if (base) fs.rmSync(base, { recursive: true, force: true });
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));

// ---- A real coordinator with hostile data -------------------------------
const HOSTILE = '<img src=x onerror="window.__pwned=1"><script>window.__pwned=2</script> & </title>';
const HOSTILE_FILE = '<b onclick=alert(1)>&"q".txt';
function git(root, ...rest) {
  const run = spawnSync('git', ['-C', root, '-c', 'user.name=LangSpan test', '-c', 'user.email=test@example.invalid', ...rest]);
  if (run.status !== 0) fail('git ' + rest.join(' ') + ': ' + run.stderr);
}
function hook(stateDir, root, vendor, event) {
  const run = spawnSync(binary, ['hook', vendor, '--state-dir', stateDir, '--workspace', 'work', '--root', root],
    { input: JSON.stringify(event) });
  if (run.status !== 0) fail('hook failed: ' + run.stderr);
}
async function coordinator() {
  base = fs.mkdtempSync(path.join(os.tmpdir(), 'ls-browser-'));
  fs.chmodSync(base, 0o700);
  const stateDir = path.join(base, 'state');
  const root = path.join(base, 'work');
  fs.mkdirSync(root);
  for (const file of ['a.txt', 'b.txt', HOSTILE_FILE]) fs.writeFileSync(path.join(root, file), 'hello\n');
  git(root, 'init', '--quiet', '--initial-branch=main');
  git(root, 'add', '.');
  git(root, 'commit', '--quiet', '-m', 'fixture');
  const daemon = spawn(binary, ['daemon', '--state-dir', stateDir], { stdio: 'ignore' });
  children.push(daemon);
  await until('the daemon', () => spawnSync(binary, ['status', '--state-dir', stateDir]).status === 0);
  const claude = '00000000-0000-4000-8000-00000000000a';
  hook(stateDir, root, 'claude-code', { session_id: claude, transcript_path: '/dev/null', cwd: root, hook_event_name: 'UserPromptSubmit', prompt: HOSTILE });
  const patch = (session, call, file) => ({
    session_id: session, turn_id: 'turn', transcript_path: null, cwd: root, hook_event_name: 'PreToolUse', model: 'example',
    permission_mode: 'default', tool_name: 'apply_patch', tool_use_id: call,
    tool_input: { command: '*** Begin Patch\n*** Update File: ' + file + '\n@@\n-hello\n+hi\n*** End Patch' },
  });
  hook(stateDir, root, 'codex', patch('11111111-0000-4000-8000-00000000000b', 'c1', HOSTILE_FILE));
  hook(stateDir, root, 'codex', patch('22222222-0000-4000-8000-00000000000c', 'c2', HOSTILE_FILE));
  hook(stateDir, root, 'codex', patch('11111111-0000-4000-8000-00000000000b', 'c3', 'a.txt'));
  for (const command of [['message', 'work', HOSTILE, HOSTILE_FILE], ['remember', 'work', HOSTILE, HOSTILE_FILE]]) {
    const run = spawnSync(binary, ['--state-dir', stateDir, ...command]);
    if (run.status !== 0) fail(command[0] + ' failed: ' + run.stderr);
  }
  return { stateDir, daemon };
}
async function dashboard(stateDir, demo) {
  const child = spawn(binary, ['dashboard', '--listen', '127.0.0.1:0', '--state-dir', stateDir, ...(demo ? ['--demo'] : [])],
    { stdio: ['ignore', 'pipe', 'ignore'] });
  children.push(child);
  let line = '';
  const url = await until('the dashboard URL', async () => {
    const chunk = child.stdout.read();
    if (chunk) line += chunk;
    return line.includes('\n') ? line.trim() : null;
  });
  return { url, origin: new URL(url).origin, child };
}

// ---- A headless browser over the DevTools protocol ---------------------------
async function browser() {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'ls-browser-profile-'));
  const proc = spawn(browserPath, ['--headless=new', '--remote-debugging-port=0', '--user-data-dir=' + profile,
    '--no-first-run', '--no-default-browser-check', '--disable-extensions', '--disable-background-networking',
    '--disable-sync', '--disable-component-update', '--hide-scrollbars', 'about:blank'], { stdio: 'ignore' });
  children.push(proc);
  const portFile = path.join(profile, 'DevToolsActivePort');
  const [port, wsPath] = (await until('the browser', () => fs.existsSync(portFile) && fs.readFileSync(portFile, 'utf8').split('\n')[1]
    ? fs.readFileSync(portFile, 'utf8').split('\n') : null, 20000));
  const ws = new WebSocket('ws://127.0.0.1:' + port + wsPath);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let next = 1;
  const pending = new Map();
  const listeners = [];
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) reject(new Error(JSON.stringify(message.error))); else resolve(message.result);
    } else if (message.method) {
      for (const listener of listeners) listener(message);
    }
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = next++;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  return { send, listeners, close: () => { ws.close(); proc.kill('SIGKILL'); fs.rmSync(profile, { recursive: true, force: true }); } };
}
async function page(b, origin) {
  const { targetId } = await b.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await b.send('Target.attachToTarget', { targetId, flatten: true });
  const p = { sessionId, requests: [], errors: [], console: [] };
  p.send = (method, params) => b.send(method, params, sessionId);
  b.listeners.push((m) => {
    if (m.sessionId !== sessionId) return;
    if (m.method === 'Network.requestWillBeSent') p.requests.push(m.params.request.url);
    if (m.method === 'Runtime.exceptionThrown') p.errors.push(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text);
    if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error') p.console.push(m.params.entry.text);
    if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') p.console.push(JSON.stringify(m.params.args.map((a) => a.value)));
  });
  for (const domain of ['Page', 'Runtime', 'Network', 'Log']) await p.send(domain + '.enable');
  p.eval = async (expression) => {
    const result = await p.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  };
  p.size = async (width, theme) => {
    await p.send('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: width < 768 });
    await p.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: theme }, { name: 'prefers-reduced-motion', value: 'reduce' }] });
  };
  p.go = async (url) => {
    await p.send('Page.navigate', { url });
    await until('the page', () => p.eval("document.readyState === 'complete'"));
  };
  p.shot = async (file) => {
    const { cssContentSize } = await p.send('Page.getLayoutMetrics');
    const shot = await p.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true,
      clip: { x: 0, y: 0, width: Math.ceil(cssContentSize.width), height: Math.min(Math.ceil(cssContentSize.height), 6000), scale: 1 } });
    fs.writeFileSync(path.join(out, file), Buffer.from(shot.data, 'base64'));
  };
  p.foreign = () => p.requests.filter((url) => !url.startsWith(origin + '/') && !url.startsWith('data:') && url !== 'about:blank');
  return p;
}

// ---- Accessibility and layout rules run in the page ---------------------------
const AUDIT = String.raw`(() => {
  const problems = [];
  const visible = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  const name = (el) => (el.getAttribute('aria-label') || el.textContent || '').trim()
    || (el.id && document.querySelector('label[for="' + el.id + '"]')?.textContent.trim()) || el.getAttribute('title') || el.getAttribute('placeholder');
  for (const el of document.querySelectorAll('button, input, select, a, [tabindex]')) {
    if (!name(el) && !(el.getAttribute('aria-labelledby'))) problems.push('no accessible name: ' + el.outerHTML.slice(0, 80));
  }
  for (const el of document.querySelectorAll('input, select')) {
    if (!(el.id && document.querySelector('label[for="' + el.id + '"]')) && !el.getAttribute('aria-label')) problems.push('no label: #' + el.id);
  }
  const ids = [...document.querySelectorAll('[id]')].map((el) => el.id);
  for (const id of new Set(ids)) if (ids.filter((x) => x === id).length > 1) problems.push('duplicate id: ' + id);
  if (document.querySelectorAll('h1').length !== 1) problems.push('expected one h1');
  let level = 0;
  for (const h of document.querySelectorAll('h1, h2, h3, h4')) { const n = Number(h.tagName[1]); if (n > level + 1) problems.push('heading skips to ' + h.tagName + ': ' + h.textContent.slice(0, 40)); level = n; }
  if (!document.documentElement.lang) problems.push('no lang');
  if (!document.querySelector('main')) problems.push('no main landmark');
  const parse = (c) => { const m = c.match(/rgba?\(([^)]+)\)/); if (!m) return null; const p = m[1].split(',').map(Number); return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 }; };
  const lum = ({ r, g, b }) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
  const background = (el) => { for (let n = el; n; n = n.parentElement) { const c = parse(getComputedStyle(n).backgroundColor); if (c && c.a > 0.5) return c; } return parse(getComputedStyle(document.body).backgroundColor); };
  let checked = 0;
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const seen = new Set();
  while (walker.nextNode()) {
    const el = walker.currentNode.parentElement;
    if (!el || seen.has(el) || !walker.currentNode.textContent.trim() || !visible(el) || el.closest('.is-stale, [hidden], template, .sr-only')) continue;
    seen.add(el);
    const s = getComputedStyle(el);
    const fg = parse(s.color), bg = background(el);
    if (!fg || !bg) continue;
    const ratio = (Math.max(lum(fg), lum(bg)) + 0.05) / (Math.min(lum(fg), lum(bg)) + 0.05);
    const large = parseFloat(s.fontSize) >= 24 || (parseFloat(s.fontSize) >= 18.66 && Number(s.fontWeight) >= 700);
    checked++;
    if (ratio < (large ? 3 : 4.5)) problems.push('contrast ' + ratio.toFixed(2) + ' for "' + walker.currentNode.textContent.trim().slice(0, 30) + '" (' + s.color + ' on rgb(' + bg.r + ',' + bg.g + ',' + bg.b + '))');
  }
  const overflow = document.documentElement.scrollWidth - window.innerWidth;
  if (overflow > 1) problems.push('page scrolls horizontally by ' + overflow + ' px');
  const small = [...document.querySelectorAll('button')].filter((b) => visible(b) && !b.closest('.scope')).filter((b) => { const r = b.getBoundingClientRect(); return r.height < 24 || r.width < 24; });
  for (const b of small) problems.push('target under 24 px: ' + (b.textContent || b.getAttribute('aria-label')).trim().slice(0, 30));
  return { problems: [...new Set(problems)], checked };
})()`;

async function audit(p, label, axe) {
  const result = await p.eval(AUDIT);
  check(result.problems.length === 0 && result.checked > 20, label + ': accessibility and layout rules (' + result.checked + ' text nodes)' + (result.problems.length ? '\n     ' + result.problems.slice(0, 12).join('\n     ') : ''));
  if (axe) {
    await p.eval(fs.readFileSync(axe, 'utf8'));
    const violations = await p.eval("axe.run(document, { resultTypes: ['violations'] }).then((r) => r.violations.map((v) => v.id + ': ' + v.nodes.length))");
    check(violations.length === 0, label + ': axe-core' + (violations.length ? ' ' + violations.join(', ') : ''));
  }
}

// ---- The run ----------------------------------------------------------------
async function main() {
  if (!fs.existsSync(binary)) fail('no binary at ' + binary + '; build it first');
  const b = await browser();
  try {
    if (args['state-dir']) {
      // Read-only: only `langspan dashboard` against an existing coordinator.
      const live = await dashboard(path.resolve(String(args['state-dir'])), false);
      const p = await page(b, live.origin);
      for (const theme of THEMES) {
        for (const width of WIDTHS) {
          await p.size(width, theme);
          await p.go(live.url);
          await until('a view', () => p.eval("document.getElementById('status-text').textContent === 'Live'"), 15000);
          await sleep(300);
          await p.shot('real-' + theme + '-' + width + '.png');
          if (!args['screens-only']) await audit(p, 'real ' + theme + ' ' + width, args.axe);
        }
      }
      check(p.errors.length === 0, 'real: no page errors ' + p.errors.join(' | '));
      check(p.foreign().length === 0, 'real: no request beyond the page origin ' + p.foreign().join(' '));
      return;
    }
    const { stateDir, daemon } = await coordinator();
    const live = await dashboard(stateDir, false);
    const demo = await dashboard(stateDir, true);

    // The demo: every width and theme.
    const d = await page(b, demo.origin);
    for (const theme of THEMES) {
      for (const width of WIDTHS) {
        await d.size(width, theme);
        await d.go(demo.url);
        await until('the demo', () => d.eval("!!document.querySelector('.grid')"));
        const state = await d.eval("({ theme: document.documentElement.dataset.theme, label: document.getElementById('status-text').textContent, banner: document.getElementById('banner').textContent })");
        check(state.theme === theme && state.label === 'Simulated data' && state.banner.includes('Simulated'), 'demo ' + theme + ' ' + width + ': labeled simulated, theme follows the system');
        const inner = await d.eval("(() => { const s = document.getElementById('scope'); return s.scrollWidth > s.clientWidth || window.innerWidth >= 1024; })()");
        check(inner, 'demo ' + theme + ' ' + width + ': a wide matrix scrolls inside its panel');
        await d.shot('demo-' + theme + '-' + width + '.png');
        await audit(d, 'demo ' + theme + ' ' + width, args.axe);
      }
    }
    check(d.requests.every((url) => !url.includes('/api/state')), 'demo: never requests coordinator state');
    check(d.errors.length === 0, 'demo: no page errors ' + d.errors.join(' | '));
    check(d.foreign().length === 0, 'demo: no request beyond the page origin ' + d.foreign().join(' '));
    // Scenes change state without moving the selection.
    await d.size(1280, 'light');
    await d.go(demo.url);
    await until('the demo', () => d.eval("!!document.querySelector('.grid')"));
    const kept = await d.eval("(async () => { document.querySelector('.session').click(); const before = document.getElementById('inspector-subject').textContent; document.getElementById('demo-prev').click(); await new Promise((r) => setTimeout(r, 50)); return [before, document.getElementById('inspector-subject').textContent]; })()");
    check(kept[0] && kept[0] === kept[1], 'demo: a selected session stays selected across scenes (' + kept.join(' / ') + ')');

    // The live page.
    const p = await page(b, live.origin);
    await p.size(1280, 'light');
    await p.go(live.url);
    await until('a live view', () => p.eval("document.getElementById('status-text').textContent === 'Live' && !!document.querySelector('.grid')"), 15000);
    const hostile = await p.eval("({ pwned: window.__pwned === undefined, scripts: document.scripts.length, imgs: document.images.length, text: document.body.textContent })");
    check(hostile.pwned && hostile.scripts === 1 && hostile.imgs === 0, 'live: hostile task, path and message text is never markup');
    check(hostile.text.includes('onerror'), 'live: the hostile text is shown as text');
    const polled = await p.eval("performance.getEntriesByType('resource').filter((e) => e.name.includes('/api/state')).length");
    await sleep(4500);
    const polledLater = await p.eval("performance.getEntriesByType('resource').filter((e) => e.name.includes('/api/state')).length");
    check(polledLater >= polled + 2, 'live: polls /api/state about every 2 s (' + polled + ' then ' + polledLater + ')');
    const tokenHeader = p.requests.filter((url) => url.endsWith('/api/state')).length > 0;
    check(tokenHeader, 'live: requests /api/state without the token in its URL');
    // Selection, focus and scroll survive refreshes.
    const before = await p.eval("(() => { const cell = document.querySelector('.cell-button'); cell.click(); cell.focus(); document.getElementById('scope').scrollLeft = 40; return { subject: document.getElementById('inspector-subject').textContent, focus: document.activeElement.dataset.focus, left: document.getElementById('scope').scrollLeft }; })()");
    await sleep(4500);
    const after = await p.eval("({ subject: document.getElementById('inspector-subject').textContent, focus: document.activeElement.dataset.focus, left: document.getElementById('scope').scrollLeft })");
    check(before.subject && before.subject === after.subject, 'live: the selection survives refreshes');
    check(before.focus && before.focus === after.focus, 'live: keyboard focus survives refreshes');
    check(before.left === after.left, 'live: the matrix scroll position survives refreshes');
    const collapsed = await p.eval("(() => { document.querySelector('.group-button').click(); return document.querySelector('.group-button').getAttribute('aria-expanded'); })()");
    check(collapsed === 'false', 'live: a workspace group collapses');
    await p.eval("document.querySelector('.group-button').click()");
    const filtered = await p.eval("(() => { const s = document.getElementById('path-view'); s.value = 'all'; s.dispatchEvent(new Event('change')); return document.querySelectorAll('.path-head').length; })()");
    check(filtered >= 2, 'live: all observed paths widen the matrix (' + filtered + ' paths)');
    for (const theme of THEMES) {
      for (const width of [390, 1280]) {
        await p.size(width, theme);
        await p.go(live.url);
        await until('a live view', () => p.eval("document.getElementById('status-text').textContent === 'Live'"), 15000);
        await p.shot('live-' + theme + '-' + width + '.png');
        await audit(p, 'live ' + theme + ' ' + width, args.axe);
      }
    }
    check(p.errors.length === 0, 'live: no page errors ' + p.errors.join(' | '));
    check(p.foreign().length === 0, 'live: no request beyond the page origin ' + p.foreign().join(' '));
    check(p.console.filter((text) => /Content Security Policy|Refused to/.test(text)).length === 0, 'live: no content security policy violation ' + p.console.join(' | '));

    // A wrong token: the page says so.
    await p.go(live.origin + '/?t=' + '0'.repeat(64));
    const wrong = await p.eval('document.body.textContent');
    check(wrong.includes('wrong access token'), 'live: a wrong token is refused before the page loads');
    // The coordinator stops: the last view stays, dimmed and labeled stale.
    await p.size(1280, 'light');
    await p.go(live.url);
    await until('a live view', () => p.eval("document.getElementById('status-text').textContent === 'Live'"), 15000);
    daemon.kill('SIGKILL');
    await until('the stale state', () => p.eval("document.getElementById('status-text').textContent === 'Stale'"), 15000);
    const stale = await p.eval("({ banner: document.getElementById('banner').textContent, dim: document.getElementById('scope').classList.contains('is-stale'), kept: !!document.querySelector('.grid') })");
    check(stale.banner.includes('Coordinator unavailable') && stale.banner.includes('Showing what was known') && stale.dim && stale.kept,
      'live: a 503 keeps the last view, dims it and says since when');
    await p.shot('live-stale-1280.png');
  } finally {
    b.close();
  }
}

main().then(() => {
  console.log(failures.length ? failures.length + ' check(s) failed' : 'all checks passed; screenshots in ' + out);
  process.exit(failures.length ? 1 : 0);
}, (error) => { console.error(error); process.exit(1); });
