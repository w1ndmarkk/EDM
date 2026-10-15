// HLS playlist reading, shared by the content script (runs in the player's frame, so requests look like the
// player's own) and the background worker (fallback).
// eslint-disable-next-line no-var
var EDM_HLS = (() => {
  'use strict';

  function attrs(s) {
    const out = {};
    const re = /([A-Z0-9-]+)=("[^"]*"|[^,]*)/g;
    let m;
    while ((m = re.exec(s))) out[m[1]] = m[2].replace(/^"|"$/g, '');
    return out;
  }

  /** Master playlist → variants/subtitles; media playlist → duration. Null when the text is not a playlist. */
  function parse(text, base) {
    text = String(text || '').replace(/^﻿/, '');
    if (!text.trimStart().startsWith('#EXTM3U')) return null;
    const lines = text.split(/\r?\n/).map((l) => l.trim());
    const out = { variants: [], subtitles: [], duration: 0, segments: 0, encrypted: '', live: true };
    for (let i = 0; i < lines.length; i++) {
      const l = lines[i];
      if (l.startsWith('#EXT-X-STREAM-INF:')) {
        const a = attrs(l.slice(18));
        let j = i + 1;
        while (j < lines.length && (!lines[j] || lines[j].startsWith('#'))) j++;
        if (j < lines.length) {
          const res = /(\d+)x(\d+)/.exec(a.RESOLUTION || '');
          out.variants.push({
            url: new URL(lines[j], base).href,
            bandwidth: Number(a['AVERAGE-BANDWIDTH'] || a.BANDWIDTH || 0),
            width: res ? Number(res[1]) : 0,
            height: res ? Number(res[2]) : 0,
            codecs: a.CODECS || '',
          });
          i = j;
        }
      } else if (l.startsWith('#EXT-X-MEDIA:')) {
        const a = attrs(l.slice(13));
        if (a.TYPE === 'SUBTITLES' && a.URI) out.subtitles.push({ url: new URL(a.URI, base).href, name: a.NAME || a.LANGUAGE || '', lang: a.LANGUAGE || '' });
      } else if (l.startsWith('#EXTINF:')) {
        out.duration += parseFloat(l.slice(8)) || 0;
        out.segments++;
      } else if (l.startsWith('#EXT-X-KEY:')) {
        const method = attrs(l.slice(11)).METHOD || '';
        if (method && method !== 'NONE') out.encrypted = method;
      } else if (l.startsWith('#EXT-X-ENDLIST')) {
        out.live = false;
      }
    }
    // One entry per resolution (the highest bitrate wins).
    const byHeight = new Map();
    for (const v of out.variants) {
      const key = v.height || v.bandwidth;
      if (!byHeight.has(key) || byHeight.get(key).bandwidth < v.bandwidth) byHeight.set(key, v);
    }
    out.variants = [...byHeight.values()].sort((a, b) => (b.height - a.height) || (b.bandwidth - a.bandwidth));
    return out;
  }

  // Like the player: no cookies on cross-origin requests (CDNs answer "Access-Control-Allow-Origin: *", which a
  // credentialed request may not use); retry with cookies for servers that need them.
  async function get(url) {
    try {
      return await fetch(url, { credentials: 'same-origin', cache: 'no-store' });
    } catch {
      return fetch(url, { credentials: 'include', cache: 'no-store' });
    }
  }

  async function fetchText(url) {
    const r = await get(url);
    if (!r.ok) throw new Error('HTTP ' + r.status);
    if (Number(r.headers.get('content-length') || 0) > 4000000) throw new Error('too large');
    return r.text();
  }

  /** Reads only the first bytes: is this response (whatever its type or extension) an HLS playlist? */
  async function looksLikePlaylist(url) {
    const r = await get(url);
    if (!r.ok || !r.body) return false;
    const reader = r.body.getReader();
    const { value } = await reader.read();
    reader.cancel().catch(() => {});
    const head = new TextDecoder().decode((value || new Uint8Array()).slice(0, 64)).replace(/^﻿/, '').trimStart();
    return head.startsWith('#EXTM3U');
  }

  /** Playlist details for the download panel; for a master playlist the duration comes from its best variant. */
  async function describe(url) {
    const p = parse(await fetchText(url), url);
    if (!p) return null;
    if (p.variants.length) {
      try {
        const media = parse(await fetchText(p.variants[0].url), p.variants[0].url);
        if (media) {
          p.duration = media.duration;
          p.segments = media.segments;
          p.encrypted = media.encrypted;
          p.live = media.live;
        }
      } catch { /* duration stays unknown */ }
    }
    return p;
  }

  return { parse, describe, looksLikePlaylist };
})();
