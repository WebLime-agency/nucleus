#!/usr/bin/env node
import http from 'node:http';
import { mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { chromium } from 'playwright';

const viewportWidth = Number(process.env.NUCLEUS_BROWSER_WIDTH || 1280);
const viewportHeight = Number(process.env.NUCLEUS_BROWSER_HEIGHT || 900);
const stateRoot = resolve(process.env.NUCLEUS_STATE_DIR || join(tmpdir(), 'nucleus-browser-playwright'));
const contexts = new Map();
const streams = new Map();
let nextPage = 1;
let nextStream = 1;

function safeSessionId(value) {
  return String(value || 'default').replace(/[^a-zA-Z0-9_.-]/g, '_').slice(0, 120) || 'default';
}

async function contextFor(sessionId) {
  const key = safeSessionId(sessionId);
  if (contexts.has(key)) return contexts.get(key);
  const profileDir = join(stateRoot, 'browser', key, 'profile');
  const downloadsDir = join(stateRoot, 'browser', key, 'downloads');
  await mkdir(profileDir, { recursive: true });
  await mkdir(downloadsDir, { recursive: true });
  const launch = {
    headless: true,
    acceptDownloads: true,
    downloadsPath: downloadsDir,
    viewport: { width: viewportWidth, height: viewportHeight },
    screen: { width: viewportWidth, height: viewportHeight },
    ignoreHTTPSErrors: true,
    args: [
      '--disable-dev-shm-usage',
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-features=Translate,AutomationControlled',
      `--window-size=${viewportWidth},${viewportHeight}`
    ]
  };
  if (process.env.NUCLEUS_BROWSER_CHROME) launch.executablePath = process.env.NUCLEUS_BROWSER_CHROME;
  else launch.channel = process.env.NUCLEUS_BROWSER_CHANNEL || 'chrome';
  let context;
  try {
    context = await chromium.launchPersistentContext(profileDir, launch);
  } catch (err) {
    delete launch.channel;
    context = await chromium.launchPersistentContext(profileDir, launch);
  }
  const state = { id: key, context, activePageId: '', pageIds: new WeakMap() };
  for (const page of context.pages()) registerPage(state, page);
  if (!context.pages().length) registerPage(state, await context.newPage());
  contexts.set(key, state);
  return state;
}

function registerPage(state, page, wantedId = '') {
  if (!state.pageIds.has(page)) {
    state.pageIds.set(page, wantedId || `page-${nextPage++}`);
    page.setViewportSize({ width: viewportWidth, height: viewportHeight }).catch(() => {});
    page.on('close', () => {
      if (state.activePageId === state.pageIds.get(page)) state.activePageId = firstPage(state)?.id || '';
    });
  }
  state.activePageId ||= state.pageIds.get(page);
  return state.pageIds.get(page);
}

function pagesFor(state) {
  return state.context.pages().filter((p) => !p.isClosed()).map((page) => ({ id: registerPage(state, page), page }));
}

function firstPage(state) { return pagesFor(state)[0] || null; }

function viewportFor(page) {
  return page.viewportSize() || { width: viewportWidth, height: viewportHeight };
}

async function pageFor(state, pageId = '', create = true) {
  let found = pagesFor(state).find((entry) => entry.id === pageId);
  if (!found && !pageId && state.activePageId) found = pagesFor(state).find((entry) => entry.id === state.activePageId);
  if (!found && !pageId) found = firstPage(state);
  if (!found && create) {
    const page = await state.context.newPage();
    const id = registerPage(state, page, pageId || '');
    found = { id, page };
  }
  if (!found) throw new Error('browser page not found');
  state.activePageId = found.id;
  return found;
}

function normalizeUrl(input) {
  const value = String(input || '').trim();
  if (!value) return 'about:blank';
  if (value === 'about:blank') return value;
  return /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(value) ? value : `https://${value}`;
}

async function pageState(state, entry, includeScreenshot = false) {
  const { id, page } = entry;
  let content = '';
  let refs = [];
  try {
    const data = await page.evaluate(() => {
      const text = document.body?.innerText || '';
      const elements = [...document.querySelectorAll('a,button,input,textarea,select,[role="button"],[contenteditable="true"]')].slice(0, 80);
      return { text, refs: elements.map((el, index) => {
        const tag = el.tagName.toLowerCase();
        const id = `ref-${index + 1}`;
        const selector = el.id ? `${tag}#${CSS.escape(el.id)}` : `${tag}:nth-of-type(${index + 1})`;
        return { id, label: (el.getAttribute('aria-label') || el.getAttribute('title') || el.innerText || el.value || el.placeholder || el.tagName || '').trim().slice(0, 120), kind: tag, selector };
      }) };
    });
    content = String(data.text || '').split(/\s+/).slice(0, 1200).join(' ');
    refs = data.refs || [];
  } catch {}
  let screenshot_data_url = '';
  if (includeScreenshot) {
    try {
      const shot = await page.screenshot({ type: 'jpeg', quality: 82, fullPage: false });
      screenshot_data_url = `data:image/jpeg;base64,${shot.toString('base64')}`;
    } catch {}
  }
  return { page_id: id, url: page.url(), title: await page.title().catch(() => page.url() || 'New Tab'), loading: false, content, refs, screenshot_data_url };
}

async function contextResult(state, activeEntry = null) {
  return { page: activeEntry ? await pageState(state, activeEntry, false) : null, pages: await Promise.all(pagesFor(state).map((entry) => pageState(state, entry, false))) };
}

async function handle(body) {
  const sessionId = body.session_id || 'default';
  const state = await contextFor(sessionId);
  switch (body.op) {
    case 'open': {
      let entry = null;
      const existing = pagesFor(state);
      if (!body.new_tab && existing.length === 1 && existing[0].page.url() === 'about:blank') entry = existing[0];
      if (!entry) {
        const page = await state.context.newPage();
        entry = { id: registerPage(state, page), page };
      }
      state.activePageId = entry.id;
      return contextResult(state, entry);
    }
    case 'navigate': {
      const entry = await pageFor(state, body.page_id || '', true);
      await entry.page.goto(normalizeUrl(body.url), { waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
      await entry.page.waitForLoadState('networkidle', { timeout: 3000 }).catch(() => {});
      return pageState(state, entry, false);
    }
    case 'snapshot': return pageState(state, await pageFor(state, body.page_id || '', false), true);
    case 'select': return contextResult(state, await pageFor(state, body.page_id || '', false));
    case 'input': {
      const entry = await pageFor(state, body.page_id || '', false);
      await applyInput(entry.page, body);
      return pageState(state, entry, false);
    }
    case 'command': return command(state, body);
    case 'annotation': return { annotation: await annotation((await pageFor(state, body.page_id || '', false)).page, body.payload || body) };
    case 'start_screencast': return startScreencast(state, await pageFor(state, body.page_id || '', false), body.quality || 82);
    case 'pop_frame': return { frame: popFrame(body.stream_id) };
    case 'stop_screencast': return stopScreencast(body.stream_id);
    default: throw new Error(`unknown browser op: ${body.op}`);
  }
}

async function applyInput(page, body) {
  const action = body.action || body.type;
  if (action === 'click') await page.mouse.click(Number(body.x || 0), Number(body.y || 0), { button: body.button || 'left' });
  else if (action === 'pointer_down') { await page.mouse.move(Number(body.x || 0), Number(body.y || 0)); await page.mouse.down({ button: body.button || 'left' }); }
  else if (action === 'pointer_up') { await page.mouse.move(Number(body.x || 0), Number(body.y || 0)); await page.mouse.up({ button: body.button || 'left' }); }
  else if (action === 'pointer_move') await page.mouse.move(Number(body.x || 0), Number(body.y || 0));
  else if (action === 'wheel' || action === 'scroll') await page.mouse.wheel(Number(body.delta_x || body.deltaX || 0), Number(body.delta_y || body.deltaY || 0));
  else if (action === 'type') await page.keyboard.type(String(body.text ?? body.value ?? ''), { delay: 5 });
  else if (action === 'key' || action === 'press') await page.keyboard.press(String(body.key || body.value || 'Enter'));
}


async function command(state, body) {
  const entry = await pageFor(state, body.page_id || '', false);
  if (body.command === 'back') await entry.page.goBack({ waitUntil: 'domcontentloaded', timeout: 15000 }).catch(() => {});
  else if (body.command === 'forward') await entry.page.goForward({ waitUntil: 'domcontentloaded', timeout: 15000 }).catch(() => {});
  else if (body.command === 'reload') await entry.page.reload({ waitUntil: 'domcontentloaded', timeout: 15000 }).catch(() => {});
  else if (body.command === 'close') {
    await entry.page.close().catch(() => {});
    let fallback = firstPage(state);
    if (!fallback) {
      const page = await state.context.newPage();
      fallback = { id: registerPage(state, page), page };
    }
    state.activePageId = fallback.id;
  } else if (body.command === 'set_viewport') {
    const width = Math.max(320, Math.min(2400, Number(body.width || viewportWidth)));
    const height = Math.max(240, Math.min(1800, Number(body.height || viewportHeight)));
    await entry.page.setViewportSize({ width, height }).catch(() => {});
  }
  return contextResult(state, pagesFor(state).find((candidate) => candidate.id === state.activePageId) || firstPage(state));
}

async function annotation(page, payload) {
  const x = Number(payload.x || 0), y = Number(payload.y || 0);
  return page.evaluate(({ x, y }) => {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { tag: el.tagName.toLowerCase(), text: (el.innerText || el.value || '').trim().slice(0, 500), aria: el.getAttribute('aria-label'), title: el.getAttribute('title'), href: el.href || null, bounds: { x: r.x, y: r.y, width: r.width, height: r.height } };
  }, { x, y });
}

async function startScreencast(state, entry, quality) {
  const cdp = await state.context.newCDPSession(entry.page);
  const stream_id = `stream-${nextStream++}`;
  const stream = { stream_id, page_id: entry.id, cdp, latest: null };
  const viewport = viewportFor(entry.page);
  streams.set(stream_id, stream);
  await cdp.send('Page.enable').catch(() => {});
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: viewport.width, height: viewport.height, deviceScaleFactor: 1, mobile: false }).catch(() => {});
  await cdp.send('Page.startScreencast', { format: 'jpeg', quality, everyNthFrame: 1 });
  cdp.on('Page.screencastFrame', async (frame) => {
    stream.latest = { page_id: entry.id, mime: 'image/jpeg', image: frame.data, state: await pageState(state, entry, false).catch(() => null) };
    cdp.send('Page.screencastFrameAck', { sessionId: frame.sessionId }).catch(() => {});
  });
  return { stream_id };
}
function popFrame(streamId) { const s = streams.get(streamId); if (!s) return null; const f = s.latest; s.latest = null; return f; }
async function stopScreencast(streamId) { const s = streams.get(streamId); if (s) { await s.cdp.send('Page.stopScreencast').catch(() => {}); await s.cdp.detach().catch(() => {}); streams.delete(streamId); } return { ok: true }; }

async function readBody(req) { const chunks = []; for await (const c of req) chunks.push(c); return chunks.length ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : {}; }
const server = http.createServer(async (req, res) => {
  try {
    if (req.method !== 'POST') { res.writeHead(405); res.end(JSON.stringify({ error: 'method not allowed' })); return; }
    const path = new URL(req.url, 'http://127.0.0.1').pathname.slice(1);
    const body = await readBody(req);
    body.op ||= path;
    const result = await handle(body);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(result));
  } catch (err) {
    res.writeHead(500, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: String(err?.message || err) }));
  }
});
server.listen(0, '127.0.0.1', () => console.log(JSON.stringify({ port: server.address().port })));

process.on('SIGTERM', async () => { for (const state of contexts.values()) await state.context.close().catch(() => {}); process.exit(0); });
