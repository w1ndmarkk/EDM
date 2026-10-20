// EDM bridge extension — background script (Chrome, Edge, Brave: service worker; Firefox: background script; MV3)
'use strict';

// Firefox loads hls.js and rules.js itself (manifest "background.scripts"); a Chrome service worker imports them.
if (typeof importScripts === 'function') importScripts('hls.js', 'rules.js');

const api = typeof browser !== 'undefined' ? browser : chrome;
const t = (k, subs) => { try { return api.i18n.getMessage(k, subs) || k; } catch { return k; } };

const DEFAULTS = { port: 48213, intercept: true, minSizeMB: 0, sniff: true, panel: true };
const BROWSER = /Firefox\//.test(navigator.userAgent) ? 'firefox'
  : navigator.brave ? 'brave' : (/Edg\//.test(navigator.userAgent) ? 'edge' : 'chrome');

const MEDIA_EXT = /\.(mp4|m4v|webm|mkv|mov|avi|flv|mp3|m4a|aac|ogg|opus|wav|flac)(\?|#|$)/i;
const FRAGMENT = /\.(ts|m4s|m4f|cmfv|cmfa|fmp4|init)(\?|#|$)|[/_.-](seg|segment|chunk|frag|fragment)[-_]?\d+/i;
const IGNORED_HOSTS = /(^|\.)googlevideo\.com$|(^|\.)fbcdn\.net$|(^|\.)twimg\.com$/i;

// ---------- Settings ----------
async function getSettings() {
  const s = await api.storage.local.get('settings');
  return { ...DEFAULTS, ...(s.settings || {}) };
}
async function setSettings(patch) {
  const cur = await getSettings();
  await api.storage.local.set({ settings: { ...cur, ...patch } });
}
const base = (s) => `http://127.0.0.1:${s.port}`;
// Custom header = proof the request comes from an extension: ordinary web pages cannot send it
// without a CORS preflight, and the bridge only grants CORS to extension origins. Chrome omits the
// Origin header on extension GET requests that are covered by host permissions, so Origin alone is not enough.
const BRIDGE_HEADERS = { 'X-EDM-Extension': BROWSER };

// ---------- App communication ----------
async function ping(settings) {
  settings = settings || (await getSettings());
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 1200);
    const r = await fetch(base(settings) + '/ping?browser=' + BROWSER, { signal: ctrl.signal, cache: 'no-store', headers: BRIDGE_HEADERS });
    clearTimeout(t);
    if (!r.ok) return null;
    const app = await r.json();
    if (app && app.active > 0) startProgress();
    refreshRules().catch(() => {});
    return app;
  } catch {
    return null;
  }
}

// ---------- Download progress: toolbar badge + popup list ----------
async function bridgeJSON(path, init) {
  const settings = await getSettings();
  const r = await fetch(base(settings) + path, { cache: 'no-store', ...init, headers: { ...BRIDGE_HEADERS, ...(init?.headers || {}) } });
  return r.json();
}
async function getDownloads() {
  try { const j = await bridgeJSON('/downloads'); return (j && j.items) || []; } catch { return []; }
}
async function downloadAction(id, action) {
  try {
    const j = await bridgeJSON('/action', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id, action }) });
    tickProgress();
    return j;
  } catch (e) { return { ok: false, error: String(e) }; }
}
const ACTIVE_STATES = new Set(['downloading', 'probing']);
let progressTimer = null;
let badgeShown = false;
async function setGlobalBadge(text, color) {
  try { await api.action.setBadgeText({ text }); if (color) await api.action.setBadgeBackgroundColor({ color }); } catch {}
}
// Per-tab media counts override the global badge; while downloads run, every tab falls back to the global text.
async function useGlobalBadgeOnAllTabs(on) {
  try {
    const tabs = await api.tabs.query({});
    for (const tab of tabs) {
      if (on) { try { await api.action.setBadgeText({ tabId: tab.id, text: null }); } catch {} }
      else { const list = await readMedia(tab.id); try { await api.action.setBadgeText({ tabId: tab.id, text: list.length ? String(list.length) : '' }); } catch {} }
    }
  } catch {}
}
// Files handed to EDM, so one that EDM cannot fetch (the site's bot check, such as Cloudflare's, admits only the
// browser that passed it) goes back to the browser. Kept in session storage: the worker may restart meanwhile.
async function readHandedOver() {
  try { return (await api.storage.session.get('handedOver')).handedOver || {}; } catch { return {}; }
}
async function rememberHandOver(id, url) {
  if (!id) return;
  const map = await readHandedOver();
  map[id] = { url, time: Date.now() };
  const ids = Object.keys(map).sort((a, b) => map[a].time - map[b].time);
  while (ids.length > 50) delete map[ids.shift()];
  try { await api.storage.session.set({ handedOver: map }); } catch {}
}
async function takeBack(list) {
  const back = list.filter((i) => i.handback);
  if (!back.length) return;
  const map = await readHandedOver();
  for (const i of back) {
    const h = map[i.id];
    if (!h) continue; // not one of ours (added in the app)
    delete map[i.id];
    try { await api.storage.session.set({ handedOver: map }); } catch {}
    try { await api.downloads.download({ url: h.url, saveAs: false }); } catch {}
    downloadAction(i.id, 'remove');
  }
}

async function tickProgress() {
  const list = await getDownloads();
  await takeBack(list);
  const active = list.filter((i) => ACTIVE_STATES.has(i.state));
  if (active.length) {
    const measurable = active.filter((i) => i.total > 0);
    let text = '…';
    if (active.length > 1) text = active.length + '↓';
    else if (measurable.length) {
      const total = measurable.reduce((a, i) => a + i.total, 0);
      const got = measurable.reduce((a, i) => a + Math.min(i.received, i.total), 0);
      text = Math.round((100 * got) / total) + '%';
    }
    if (!badgeShown) { badgeShown = true; await useGlobalBadgeOnAllTabs(true); }
    await setGlobalBadge(text, '#2f6feb');
  } else {
    stopProgress();
    if (badgeShown) {
      badgeShown = false;
      await setGlobalBadge('✓', '#2ebd59');
      setTimeout(async () => { await setGlobalBadge('', '#2f6feb'); await useGlobalBadgeOnAllTabs(false); }, 4000);
    }
  }
}
function startProgress() {
  if (progressTimer) return;
  tickProgress();
  progressTimer = setInterval(tickProgress, 1500);  // each fetch keeps the MV3 worker alive while something runs
}
function stopProgress() {
  if (progressTimer) { clearInterval(progressTimer); progressTimer = null; }
}

async function cookiesFor(url) {
  try {
    const list = await api.cookies.getAll({ url });
    return {
      header: list.map((c) => `${c.name}=${c.value}`).join('; '),
      list: list.map((c) => ({
        name: c.name, value: c.value, domain: c.domain, path: c.path,
        secure: !!c.secure, expirationDate: c.expirationDate || 0,
      })),
    };
  } catch {
    return { header: '', list: [] };
  }
}

function notify(title, message) {
  try {
    api.notifications?.create?.('', {
      type: 'basic', iconUrl: 'icons/icon128.png', title, message: String(message || '').slice(0, 200),
    });
  } catch {}
}

// When the app last failed to answer: downloads the browser has started are left to it for a while.
let lastAppFailure = 0;

// No notification when a download starts: the app's download window is the confirmation. Only a failure to reach
// the app is reported, and not for the in-page panel, which says so itself.
async function sendToApp({ url, filename, referer, kind, format, audio, title, manifest, quiet, headers }) {
  const settings = await getSettings();
  // A blob: playlist travels as text; its segments live on the page's server (and so do its cookies).
  const cookieUrl = /^https?:/i.test(url) ? url : referer;
  const ck = cookieUrl ? await cookiesFor(cookieUrl) : { header: '', list: [] };
  // The browser's own request headers win where it sent them (exact Cookie, Referer and User-Agent).
  const extra = {};
  for (const [k, v] of Object.entries(headers || {})) if (!/^(cookie|referer|user-agent)$/i.test(k)) extra[k] = v;
  // With the browser's headers, its Referer exactly (none when it sent none); never an extension or about: address.
  const ref = headers ? headerOf(headers, 'referer') : referer;
  const payload = {
    url, filename: filename || '', referer: /^https?:/i.test(ref || '') ? ref : '', kind: kind || 'file',
    userAgent: headerOf(headers, 'user-agent') || navigator.userAgent,
    cookieHeader: headerOf(headers, 'cookie') || ck.header, cookies: ck.list,
    headers: Object.keys(extra).length ? extra : null,
    format: format || null, audio: audio || null, title: title || null, manifest: manifest || null,
  };
  try {
    const r = await fetch(base(settings) + '/download', {
      method: 'POST', headers: { 'Content-Type': 'application/json', ...BRIDGE_HEADERS }, body: JSON.stringify(payload),
    });
    const j = await r.json();
    if (!j.ok) throw new Error(j.error || 'app error');
    lastAppFailure = 0;
    if (kind !== 'video') rememberHandOver(j.id, url);
    startProgress();
    return { ok: true };
  } catch (e) {
    lastAppFailure = Date.now();
    if (!quiet) notify(t('notifOfflineTitle'), t('notifOfflineBody'));
    return { ok: false, error: String(e) };
  }
}

/** Opens the app's playlist dialog with this address (YouTube playlists and channel video pages). */
async function openPlaylistInApp(url) {
  try {
    const j = await bridgeJSON('/playlist', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ url }) });
    return { ok: !!(j && j.ok) };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

// ---------- Download interception ----------
// The app decides (Settings ▸ File Types: file types, excluded sites, excluded addresses). Any failure — the app
// is not running, an old version — leaves the download to the browser.
async function shouldCapture(url, filename) {
  try {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 1500);
    const j = await bridgeJSON('/check', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ url, filename }), signal: ctrl.signal,
    });
    clearTimeout(timer);
    return !!(j && j.ok && j.capture);
  } catch {
    return false;
  }
}

// The rules themselves, kept here so a decision needs no round trip: the browser keeps downloading while it waits
// for one, and on a metered connection every byte counts. Refreshed from the app at most once a minute.
let rulesCache = null; // { rules, time }
async function loadRules() {
  if (!rulesCache) {
    try { rulesCache = (await api.storage.local.get('captureRules')).captureRules || null; } catch {}
  }
  return rulesCache;
}
async function refreshRules(force) {
  const cur = await loadRules();
  if (!force && cur && Date.now() - cur.time < 60 * 1000) return cur;
  try {
    const j = await bridgeJSON('/rules'); // older apps answer "not found": decisions then go through /check
    if (j && j.ok && j.rules) {
      rulesCache = { rules: j.rules, time: Date.now() };
      await api.storage.local.set({ captureRules: rulesCache });
    }
  } catch { /* the app is not running: keep the last rules */ }
  return rulesCache;
}
async function decide(url, filename) {
  const cache = await loadRules();
  return cache ? EDM_RULES.capture(cache.rules, url, filename) : shouldCapture(url, filename);
}
const tooSmall = (settings, bytes) => settings.minSizeMB > 0 && bytes > 0 && bytes < settings.minSizeMB * 1024 * 1024;

// ---------- The browser's own request headers ----------
// Sites behind bot protection (Cloudflare) let a request through when it looks like the browser's: same User-Agent
// and the headers that go with it (Accept, Accept-Language, sec-ch-ua, sec-fetch-*). EDM repeats the headers the
// browser sent for the file, as IDM does. Kept for a few minutes per address; each tab's last page load serves as
// the pattern for a link click, whose request the browser never sent.
const sentHeaders = new Map(); // url -> { headers, time }
const pageHeaders = new Map(); // tabId -> headers of the tab's last page load
// Set by EDM itself, or meaningless for its segmented requests.
const NOT_FORWARDED = /^(host|connection|keep-alive|proxy-.*|content-length|transfer-encoding|te|upgrade|range|if-.*|accept-encoding|cache-control|pragma|priority)$/i;
function keepHeaders(list) {
  const out = {};
  for (const h of list || []) {
    if (h && h.name && typeof h.value === 'string' && !NOT_FORWARDED.test(h.name)) out[h.name] = h.value;
  }
  return out;
}
if (api.webRequest?.onSendHeaders) {
  // Chrome shows Cookie and Referer only with "extraHeaders"; Firefox shows everything and rejects that option.
  const options = BROWSER === 'firefox' ? ['requestHeaders'] : ['requestHeaders', 'extraHeaders'];
  try {
    api.webRequest.onSendHeaders.addListener((d) => {
      if (d.tabId < 0) return;
      const headers = keepHeaders(d.requestHeaders);
      sentHeaders.delete(d.url);
      sentHeaders.set(d.url, { headers, time: Date.now() });
      while (sentHeaders.size > 60) sentHeaders.delete(sentHeaders.keys().next().value);
      if (d.type === 'main_frame') pageHeaders.set(d.tabId, headers);
    }, { urls: ['<all_urls>'], types: ['main_frame', 'sub_frame'] }, options);
  } catch { /* EDM falls back to its own few headers */ }
}
api.tabs.onRemoved.addListener((tabId) => pageHeaders.delete(tabId));

const headerOf = (headers, name) => {
  const key = Object.keys(headers || {}).find((k) => k.toLowerCase() === name);
  return key ? headers[key] : '';
};
// "a.b.example.com" → "example.com" (close enough for sec-fetch-site; no public-suffix list here).
const siteOf = (host) => host.split('.').slice(-2).join('.');

/** The headers the browser sent for this address, or the tab's page load re-aimed at it (a link click on `from`). */
function browserHeaders(url, tabId, from) {
  const hit = sentHeaders.get(url);
  if (hit && Date.now() - hit.time < 5 * 60 * 1000) return hit.headers;
  const page = tabId != null ? pageHeaders.get(tabId) : null;
  if (!page) return null;
  const h = {};
  for (const [k, v] of Object.entries(page)) if (!/^(cookie|referer)$/i.test(k)) h[k] = v;
  const fetchSite = Object.keys(h).find((k) => k.toLowerCase() === 'sec-fetch-site');
  if (fetchSite) {
    // The click is a navigation from the page to the file.
    let site = 'cross-site';
    try {
      const a = new URL(from), b = new URL(url);
      site = a.origin === b.origin ? 'same-origin' : siteOf(a.hostname) === siteOf(b.hostname) ? 'same-site' : 'cross-site';
    } catch { /* keep cross-site */ }
    h[fetchSite] = site;
  }
  // The Referer a browser sends for the click (default policy "strict-origin-when-cross-origin"): the page address
  // on the same origin, only its origin elsewhere, nothing from HTTPS to HTTP.
  try {
    const a = new URL(from), b = new URL(url);
    if (/^https?:$/.test(a.protocol) && !(a.protocol === 'https:' && b.protocol === 'http:')) {
      h.Referer = a.origin === b.origin ? a.href : a.origin + '/';
    }
  } catch { /* no referer */ }
  return h;
}

/** The app takes the download; when it is not running, the browser downloads the file after all. */
async function handOver(url, filename, referer, headers) {
  const r = await sendToApp({ url, filename, referer, kind: 'file', quiet: true, headers });
  if (!r.ok) {
    try { await api.downloads.download({ url, saveAs: false }); } catch {}
  }
  return r.ok;
}

/** A bot-check page instead of the file (mirrors segmented::bot_check in the app). */
function isBotCheck(resp) {
  if (resp.headers.get('cf-mitigated')) return true;
  const server = (resp.headers.get('server') || '').toLowerCase();
  return (resp.status === 403 || resp.status === 503) && /text\/html/i.test(resp.headers.get('content-type') || '')
    && /cloudflare|ddos-guard/.test(server);
}

// 1) A click on a download link (content.js): taken before the browser sends the request at all. A HEAD request
//    (headers only) tells a file from a page with a file-like address, such as GitHub's ".../blob/x.zip" view.
async function takeLink(msg, tabId) {
  const settings = await getSettings();
  if (!settings.intercept) return { navigate: true };
  let head = null;
  try {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 8000);
    head = await fetch(msg.url, { method: 'HEAD', credentials: 'include', cache: 'no-store', signal: ctrl.signal });
    clearTimeout(timer);
  } catch { /* no answer to HEAD: trust the address */ }
  // Not there: the browser shows the server's answer (a 403 or 405 can be a signed or GET-only address: trusted).
  if (head && (head.status === 404 || head.status === 410)) return { navigate: true };
  // A bot check (Cloudflare "Just a moment…"): the browser passes it first and then asks for the file again, with
  // the clearance cookie; that request is the one handed over (like IDM, which also takes it at that point).
  if (head && isBotCheck(head)) return { navigate: true };
  if (head && head.ok) {
    const ct = head.headers.get('content-type') || '';
    const cd = head.headers.get('content-disposition') || '';
    if (!/^\s*attachment/i.test(cd) && /text\/html|application\/xhtml/i.test(ct)) return { navigate: true };
    if (tooSmall(settings, Number(head.headers.get('content-length') || 0))) return { navigate: true };
  }
  await handOver(msg.url, msg.filename || '', msg.referer, browserHeaders(msg.url, tabId, msg.referer));
  return { taken: true };
}

// 2) Firefox: every other download (scripts, redirects, typed addresses) is stopped at its response headers,
//    before the body arrives. Firefox still allows blocking webRequest; Chrome does not.
async function takeResponse(d) {
  if (d.tabId < 0 || d.method !== 'GET' || d.statusCode < 200 || d.statusCode >= 300 || !/^https?:/i.test(d.url)) return {};
  const cd = headerVal(d.responseHeaders, 'content-disposition');
  const ct = headerVal(d.responseHeaders, 'content-type');
  const filename = EDM_RULES.dispositionName(cd);
  const ext = EDM_RULES.extension(filename) || EDM_RULES.urlExtension(d.url);
  // Only what the browser would download: an attachment, or a type it cannot show.
  const shown = ct ? EDM_RULES.shownType(ct) : EDM_RULES.shownExtension(ext);
  if (!/^\s*attachment/i.test(cd) && shown) return {};
  const settings = await getSettings();
  if (!settings.intercept || tooSmall(settings, parseInt(headerVal(d.responseHeaders, 'content-length'), 10) || 0)) return {};
  if (Date.now() - lastAppFailure < 15 * 1000) return {}; // EDM did not answer a moment ago: the browser keeps it
  if (!(await decide(d.url, filename))) return {};
  // Stop the response right away, hand it over afterwards: every millisecond spent waiting lets data through.
  const from = d.originUrl || d.documentUrl || '';
  handOver(d.url, filename, from, browserHeaders(d.url, d.tabId, from)).catch(() => {});
  return { cancel: true };
}
// 2a) Firefox: a page navigating to a file address (a script setting location, a redirect, a typed address) is
//     stopped before the request is sent, then checked like a link click. A page after all is loaded again.
const passThrough = new Set(); // addresses a HEAD request showed to be pages
async function takeRequest(d) {
  if (d.tabId < 0 || d.method !== 'GET' || !/^https?:/i.test(d.url)) return {};
  if (passThrough.delete(d.url)) return {};
  const ext = EDM_RULES.urlExtension(d.url);
  if (!ext || EDM_RULES.shownExtension(ext)) return {};
  const settings = await getSettings();
  if (!settings.intercept || Date.now() - lastAppFailure < 15 * 1000) return {};
  const cache = await loadRules();
  if (!cache || !EDM_RULES.capture(cache.rules, d.url, '')) return {};
  takeLink({ url: d.url, filename: '', referer: d.originUrl || d.documentUrl || '' }, d.tabId).then((r) => {
    if (r && r.navigate) {
      passThrough.add(d.url);
      api.tabs.update(d.tabId, { url: d.url }).catch(() => {});
    }
  }, () => {});
  return { cancel: true };
}
if (BROWSER === 'firefox') {
  try {
    api.webRequest.onBeforeRequest.addListener(takeRequest, { urls: ['<all_urls>'], types: ['main_frame'] }, ['blocking']);
    api.webRequest.onHeadersReceived.addListener(takeResponse, { urls: ['<all_urls>'], types: ['main_frame', 'sub_frame'] }, ['blocking', 'responseHeaders']);
  } catch { /* no webRequestBlocking: the downloads hooks below remain */ }
}

// 3) Downloads the browser started anyway (Chrome/Edge/Brave: script-started ones; Firefox: whatever 2 let through).
//    The browser is already receiving data, so it is stopped first, without waiting for anything but the stored
//    rules, then handed over.
async function interceptDownload(item) {
  if (item.byExtensionId) return false; // ours: handOver's fallback, the popup
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return false;
  const settings = await getSettings();
  if (!settings.intercept || tooSmall(settings, item.totalBytes)) return false;
  // EDM did not answer a moment ago: cancelling would only make the browser start over.
  if (Date.now() - lastAppFailure < 15 * 1000) return false;
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : '';
  if (!(await decide(url, filename))) return false;

  try { await api.downloads.cancel(item.id); } catch {}
  try { await api.downloads.erase({ id: item.id }); } catch {}
  await handOver(url, filename, item.referrer, browserHeaders(url) || browserHeaders(item.url));
  return true;
}

if (api.downloads.onDeterminingFilename) {
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    interceptDownload(item).then((took) => { if (!took) { try { suggest(); } catch {} } }, () => { try { suggest(); } catch {} });
    return true;
  });
} else {
  api.downloads.onCreated.addListener((item) => {
    if (item.state && item.state !== 'in_progress') return;
    interceptDownload(item).catch(() => {});
  });
}

// ---------- Formats for the in-page "Download this video" panel ----------
// Read as soon as a player appears (like IDM), so the list is ready when the panel is clicked. One request per page;
// the app keeps its own cache too, for when this worker has been asleep.
const formatsCache = new Map(); // url -> { time, promise }
const FORMATS_TTL = 30 * 60 * 1000;
function cachedFormats(url, referer) {
  const hit = formatsCache.get(url);
  if (hit && Date.now() - hit.time < FORMATS_TTL) return hit.promise;
  const promise = videoFormats(url, referer).then((r) => {
    if (!r || !r.ok) formatsCache.delete(url); // offline or failed: ask again next time
    return r;
  });
  formatsCache.set(url, { time: Date.now(), promise });
  while (formatsCache.size > 40) formatsCache.delete(formatsCache.keys().next().value);
  return promise;
}

async function videoFormats(url, referer) {
  const ck = await cookiesFor(url);
  try {
    return await bridgeJSON('/formats', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, referer: referer || url, userAgent: navigator.userAgent, cookies: ck.list }),
    });
  } catch (e) {
    // "Not running" only when the app really does not answer; otherwise the request itself failed.
    return { ok: false, offline: !(await ping()), error: String(e) };
  }
}

// ---------- Media sniffing ----------
const mem = new Map(); // tabId -> Map(url -> info)   (fallback when storage.session is unavailable)
// Media list updates of one tab run one after another: concurrent read-modify-writes used to drop entries.
const tabQueues = new Map();
function withTab(tabId, fn) {
  const next = (tabQueues.get(tabId) || Promise.resolve()).then(fn, fn);
  tabQueues.set(tabId, next.catch(() => {}));
  return next;
}

async function readMedia(tabId) {
  if (api.storage.session) {
    const key = 'media:' + tabId;
    const r = await api.storage.session.get(key);
    return r[key] || [];
  }
  return Array.from((mem.get(tabId) || new Map()).values());
}
async function writeMedia(tabId, list) {
  if (api.storage.session) {
    await api.storage.session.set({ ['media:' + tabId]: list });
  } else {
    mem.set(tabId, new Map(list.map((m) => [m.url, m])));
  }
  try {
    await api.action.setBadgeText({ tabId, text: list.length ? String(list.length) : '' });
    await api.action.setBadgeBackgroundColor({ tabId, color: '#2f6feb' });
  } catch {}
}
async function clearMedia(tabId) {
  probed.delete(tabId);
  await withTab(tabId, async () => {
    if (api.storage.session) await api.storage.session.remove('media:' + tabId);
    mem.delete(tabId);
  });
  try { await api.action.setBadgeText({ tabId, text: '' }); } catch {}
}

function normalizeKey(url) {
  try {
    const u = new URL(url);
    ['range', 'bytes', 'start', 'end', 'rn', 'rbuf', 'r', '_'].forEach((p) => u.searchParams.delete(p));
    return u.origin + u.pathname + (u.searchParams.toString() ? '?' + u.searchParams.toString() : '');
  } catch {
    return url;
  }
}

async function addMedia(tabId, info) {
  if (tabId < 0) return;
  const settings = await getSettings();
  if (!settings.sniff) return;
  await withTab(tabId, async () => {
    const list = await readMedia(tabId);
    const key = normalizeKey(info.url);
    const idx = list.findIndex((m) => m.key === key);
    const entry = { ...info, key, name: info.name || guessName(info.url, info.type) };
    if (idx >= 0) {
      const old = list[idx];
      const better = (info.size || 0) > (old.size || 0) || (info.details && !old.details) || (info.manifest && !old.manifest);
      if (better) {
        // Keep what the earlier sighting knew (the network saw the size, the page hook read the playlist).
        list[idx] = { ...old, ...entry, details: entry.details || old.details, manifest: entry.manifest || old.manifest,
          size: Math.max(info.size || 0, old.size || 0) };
        await writeMedia(tabId, list);
      }
      return;
    }
    list.unshift(entry);
    // Streams and subtitles are what the panel is for: drop plain media files first when the list is full.
    while (list.length > 60) {
      const i = list.map((m) => m.kind).lastIndexOf('media');
      list.splice(i >= 0 ? i : list.length - 1, 1);
    }
    await writeMedia(tabId, list);
  });
  // A new stream: read its resolutions and duration right away, so the panel opens with them.
  if (info.kind === 'hls' && !info.details) scheduleDescribe(tabId);
  // Audio without a visible player gets a panel docked at the bottom of the page.
  if ((info.kind === 'media' && isAudio(info)) || (info.kind === 'hls' && isAudioOnlyHls(info.details))) announceAudio(tabId);
}

const AUDIO_EXT = /\.(mp3|m4a|aac|ogg|oga|opus|wav|flac|weba|wma)(\?|#|$)/i;
const isAudio = (m) => /^audio\//i.test(m.type || '') || (!/^video\//i.test(m.type || '') && AUDIO_EXT.test(m.url));
/** An HLS stream with only audio renditions (no resolution, no video codec). */
const isAudioOnlyHls = (d) => !!(d && d.variants && d.variants.length
  && d.variants.every((v) => !v.height && !/avc|hvc|hev|vp0?8|vp0?9|av01/i.test(v.codecs || '')));
function announceAudio(tabId) {
  try { api.tabs.sendMessage(tabId, { type: 'audioCaptured' }, { frameId: 0 }).catch(() => {}); } catch {}
}

const describeTimers = new Map();
function scheduleDescribe(tabId) {
  clearTimeout(describeTimers.get(tabId));
  describeTimers.set(tabId, setTimeout(() => { describeTimers.delete(tabId); describeTabMedia(tabId).catch(() => {}); }, 400));
}

// One reading per tab at a time: a later call waits for the running one, then picks up only what is still missing.
const describing = new Map(); // tabId -> promise
function describeTabMedia(tabId) {
  const next = (describing.get(tabId) || Promise.resolve()).catch(() => {}).then(() => describeTabMediaNow(tabId));
  describing.set(tabId, next);
  next.finally(() => { if (describing.get(tabId) === next) describing.delete(tabId); }).catch(() => {});
  return next;
}

/** Fills in HLS details (resolutions, duration) for the panel, reading the playlist in the frame that loaded it. */
async function describeTabMediaNow(tabId) {
  const list = await readMedia(tabId);
  const todo = list.filter((m) => m.kind === 'hls' && m.details === undefined).slice(0, 8);
  if (!todo.length) return list;
  const timeout = (ms) => new Promise((r) => setTimeout(() => r(null), ms));
  const found = await Promise.all(todo.map(async (m) => {
    let d = null;
    try { d = await Promise.race([api.tabs.sendMessage(tabId, { type: 'parseHls', url: m.url }, { frameId: Math.max(0, m.frameId || 0) }), timeout(6000)]); } catch {}
    if (!d) { try { d = await Promise.race([EDM_HLS.describe(m.url), timeout(6000)]); } catch {} }
    return [m.key, d || null];
  }));
  return withTab(tabId, async () => {
    const current = await readMedia(tabId);
    for (const [key, d] of found) { const m = current.find((x) => x.key === key); if (m) m.details = d; }
    if (found.some(([, d]) => isAudioOnlyHls(d))) announceAudio(tabId);
    if (api.storage.session) await api.storage.session.set({ ['media:' + tabId]: current });
    else mem.set(tabId, new Map(current.map((m) => [m.url, m])));
    return current;
  });
}

// Responses whose type gives nothing away (text/plain, octet-stream) can still be HLS playlists:
// the frame that requested them reads their first bytes. Bounded per tab.
const probed = new Map(); // tabId -> Set(keys)
async function probePlaylist(d) {
  const set = probed.get(d.tabId) || new Set();
  probed.set(d.tabId, set);
  const key = normalizeKey(d.url);
  if (set.size >= 40 || set.has(key)) return;
  set.add(key);
  let isPlaylist = false;
  try {
    const r = await api.tabs.sendMessage(d.tabId, { type: 'probePlaylist', url: d.url }, { frameId: Math.max(0, d.frameId) });
    isPlaylist = !!(r && r.playlist);
  } catch {
    try { isPlaylist = await EDM_HLS.looksLikePlaylist(d.url); } catch {}
  }
  if (isPlaylist) addMedia(d.tabId, { url: d.url, type: 'application/vnd.apple.mpegurl', size: 0, kind: 'hls', frameId: d.frameId, time: Date.now() });
}

function guessName(url, type) {
  try {
    const u = new URL(url);
    let n = decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() || '') || u.hostname;
    if (!/\.[a-z0-9]{2,5}$/i.test(n) && type) {
      const ext = { 'video/mp4': 'mp4', 'video/webm': 'webm', 'audio/mpeg': 'mp3', 'audio/mp4': 'm4a', 'video/quicktime': 'mov' }[type.toLowerCase()];
      if (ext) n += '.' + ext;
    }
    return n;
  } catch {
    return url;
  }
}

function headerVal(headers, name) {
  const h = (headers || []).find((x) => x.name.toLowerCase() === name);
  return h ? h.value : '';
}
const SUBTITLE_EXT = /\.(srt|vtt|webvtt|ass|ssa|ttml|dfxp)(\?|#|$)/i;
const SUBTITLE_CT = /text\/vtt|x-subrip|application\/x-srt|text\/srt|ttml\+xml/;
// Segmented subtitles of an HLS stream (sub_00012.vtt, seg-3.vtt) are pieces, not files.
const SUBTITLE_PIECE = /(seg|segment|chunk|frag|part)[-_]?\d+[^/]*$|[-_]\d{3,}\.(vtt|webvtt)(\?|#|$)/i;
const TS_FILE = /\.ts(\?|#|$)/i;
const PLAYLIST_CANDIDATE_CT = /^(text\/plain|application\/octet-stream|binary\/octet-stream|application\/x-unknown|)$/;
const NOT_PLAYLIST = /\.(js|mjs|css|json|png|jpe?g|gif|webp|avif|svg|ico|woff2?|ttf|html?|xml|wasm|mp4|m4s|m4a|ts|aac|mp3|webm|key)(\?|#|$)/i;

function classify(url, ct, size) {
  ct = (ct || '').toLowerCase();
  if (/\.m3u8(\?|#|$)/i.test(url) || /mpegurl/.test(ct)) return 'hls';
  if (/\.mpd(\?|#|$)/i.test(url) || /dash\+xml/.test(ct)) return 'dash';
  if (SUBTITLE_EXT.test(url) || SUBTITLE_CT.test(ct)) return SUBTITLE_PIECE.test(url) ? null : 'subtitle';
  // A whole MPEG-TS file is a video; the small ones are HLS segments.
  if (TS_FILE.test(url) || /mp2t/.test(ct)) return size >= 20 * 1024 * 1024 ? 'media' : null;
  if (FRAGMENT.test(url)) return null;
  if (/^(video|audio)\//.test(ct)) {
    if (/iso\.segment|x-mpegurl/.test(ct)) return null;
    return 'media';
  }
  if (/octet-stream/.test(ct) && MEDIA_EXT.test(url)) return 'media';
  return null;
}

if (api.webRequest?.onHeadersReceived) {
  api.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (d.tabId < 0 || d.statusCode >= 400) return;
      // A new page in the tab: forget the old page's media. (Not on tabs.onUpdated: sites that change their address
      // with history.pushState — most streaming sites — would lose what the player loaded.)
      if (d.type === 'main_frame' && d.statusCode < 300 && /html/i.test(headerVal(d.responseHeaders, 'content-type'))) {
        clearMedia(d.tabId);
        return;
      }
      let host = '';
      try { host = new URL(d.url).hostname; } catch {}
      if (IGNORED_HOSTS.test(host)) return;
      const ct = headerVal(d.responseHeaders, 'content-type');
      let size = parseInt(headerVal(d.responseHeaders, 'content-length'), 10) || 0;
      const cr = headerVal(d.responseHeaders, 'content-range').match(/\/(\d+)/);
      if (cr) size = parseInt(cr[1], 10);
      // Internet radio (Icecast/SHOUTcast) never ends: not a file to download.
      if (headerVal(d.responseHeaders, 'icy-name') || headerVal(d.responseHeaders, 'icy-br') || headerVal(d.responseHeaders, 'icy-metaint')) return;
      const kind = classify(d.url, ct, size);
      if (!kind) {
        const base = ct.split(';')[0].trim().toLowerCase();
        const candidate = (d.type === 'xmlhttprequest' || d.type === 'other') && PLAYLIST_CANDIDATE_CT.test(base)
          && (size === 0 || size < 300 * 1024) && !NOT_PLAYLIST.test(d.url) && !FRAGMENT.test(d.url);
        if (candidate) probePlaylist(d);
        return;
      }
      if (kind === 'media' && size > 0 && size < 300 * 1024) return;
      addMedia(d.tabId, { url: d.url, type: ct.split(';')[0], size, kind, frameId: d.frameId, time: Date.now() });
    },
    { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other', 'object', 'main_frame', 'sub_frame'] },
    ['responseHeaders']
  );
}

api.tabs.onRemoved.addListener((tabId) => clearMedia(tabId));

// ---------- Context menu ----------
const menus = api.contextMenus || api.menus;
function createMenus() {
  if (!menus) return;
  const create = () => {
    try {
      menus.create({ id: 'dm-link', title: t('menuDownloadLink'), contexts: ['link', 'video', 'audio', 'image'] });
      menus.create({ id: 'dm-page-video', title: t('menuPageVideo'), contexts: ['page', 'video', 'frame', 'action'] });
      menus.create({
        id: 'dm-playlist', title: t('menuPlaylist'), contexts: ['page', 'link', 'video'],
        documentUrlPatterns: ['*://*.youtube.com/*'],
      });
    } catch {}
  };
  // Promise style (Firefox ignores callbacks; Chrome MV3 returns a promise too).
  try { Promise.resolve(menus.removeAll()).then(create, create); } catch { create(); }
}
api.runtime.onInstalled.addListener(() => { createMenus(); ping(); refreshRules(true).catch(() => {}); });
api.runtime.onStartup?.addListener(() => { createMenus(); ping(); refreshRules(true).catch(() => {}); });
menus?.onClicked.addListener((info, tab) => {
  if (info.menuItemId === 'dm-link') {
    const url = info.linkUrl || info.srcUrl;
    if (url) sendToApp({ url, referer: info.pageUrl || tab?.url, kind: 'file' });
  } else if (info.menuItemId === 'dm-playlist') {
    // A playlist link that was right-clicked, else the page itself.
    const link = info.linkUrl && /[?&]list=|\/playlist\b/.test(info.linkUrl) ? info.linkUrl : '';
    openPlaylistInApp(link || info.pageUrl || tab?.url).then((r) => { if (!r.ok) notify(t('notifOfflineTitle'), t('notifOfflineBody')); });
  } else if (info.menuItemId === 'dm-page-video') {
    const url = info.pageUrl || tab?.url;
    if (url) sendToApp({ url, referer: url, kind: 'video' });
  }
});

// ---------- Popup / content script messages ----------
api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  (async () => {
    try {
      switch (msg?.type) {
        case 'status': {
          const settings = await getSettings();
          const app = await ping(settings);
          sendResponse({ online: !!app, app, settings });
          break;
        }
        case 'panelSettings': sendResponse({ panel: (await getSettings()).panel }); break;
        case 'getMedia': sendResponse(await readMedia(msg.tabId)); break;
        case 'tabMedia': {
          const items = sender.tab ? await describeTabMedia(sender.tab.id) : [];
          sendResponse({ title: (sender.tab && sender.tab.title) || '', items });
          break;
        }
        case 'formats': sendResponse(await cachedFormats(msg.url, msg.referer)); break;
        case 'prefetch': {
          // A player became visible: read its qualities and the captured playlists now, not on click.
          if (typeof msg.url === 'string' && /^https?:/i.test(msg.url)) cachedFormats(msg.url, msg.referer).catch(() => {});
          if (sender.tab) describeTabMedia(sender.tab.id).catch(() => {});
          sendResponse({ ok: true });
          break;
        }
        case 'downloads': sendResponse(await getDownloads()); break;
        case 'action': sendResponse(await downloadAction(msg.id, msg.action)); break;
        case 'download': sendResponse(await sendToApp(msg)); break;
        case 'openPlaylist': sendResponse(await openPlaylistInApp(msg.url)); break;
        case 'takeLink': {
          const r = await takeLink(msg, sender.tab ? sender.tab.id : null);
          if (r.navigate && BROWSER === 'firefox') passThrough.add(msg.url); // content.js opens it now: no second check
          sendResponse(r);
          break;
        }
        case 'syncRules': refreshRules().catch(() => {}); sendResponse({ ok: true }); break;
        case 'setSettings': await setSettings(msg.settings || {}); sendResponse({ ok: true }); break;
        case 'manifest': {
          // An HLS/DASH playlist the page loaded, recognised by its content (page_hook.js) whatever its URL or type.
          if (sender.tab && (msg.kind === 'hls' || msg.kind === 'dash') && typeof msg.url === 'string') {
            const inline = !/^https?:/i.test(msg.url);
            if (!inline || msg.manifest) {
              await addMedia(sender.tab.id, {
                url: msg.url, type: msg.kind === 'hls' ? 'application/vnd.apple.mpegurl' : 'application/dash+xml', size: 0,
                kind: msg.kind, frameId: sender.frameId, time: Date.now(),
                details: msg.details || undefined, manifest: inline ? String(msg.manifest).slice(0, 8 * 1024 * 1024) : undefined,
              });
            }
          }
          sendResponse({ ok: true });
          break;
        }
        case 'pageMedia': {
          if (sender.tab) {
            for (const m of msg.items || []) {
              const kind = classify(m.url, m.type || '');
              if (kind || MEDIA_EXT.test(m.url)) {
                await addMedia(sender.tab.id, { url: m.url, type: m.type || '', size: 0, kind: kind || 'media', frameId: sender.frameId, time: Date.now(), name: m.name });
              }
            }
          }
          sendResponse({ ok: true });
          break;
        }
        default: sendResponse(null);
      }
    } catch (e) {
      sendResponse({ ok: false, error: String(e) });
    }
  })();
  return true;
});
