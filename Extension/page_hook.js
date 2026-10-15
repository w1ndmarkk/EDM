// EDM — runs in the page's own JavaScript world (manifest: "world": "MAIN"), in every frame.
// Streaming sites often hide their playlists from the network layer: disguised URLs and content types, requests
// made by a service worker, or playlists decoded in JavaScript and handed to the player as blob: URLs. This looks at
// what the page's XMLHttpRequest/fetch responses actually contain — like IDM does — and reports HLS (#EXTM3U) and
// DASH (<MPD>) manifests to content.js (isolated world) through a DOM event.
(() => {
  'use strict';
  if (window.__edmPageHook) return;
  try { Object.defineProperty(window, '__edmPageHook', { value: true }); } catch { return; }

  const EVENT = 'edm-manifest-v1';
  const MAX = 8 * 1024 * 1024;
  const seen = new Set();

  function kindOf(head) {
    head = head.replace(/^﻿/, '').trimStart();
    if (head.startsWith('#EXTM3U')) return 'hls';
    if (head.startsWith('<') && /<MPD[\s>]/.test(head)) return 'dash';
    return '';
  }

  function report(url, text) {
    if (typeof text !== 'string' || !text) return;
    const kind = kindOf(text.slice(0, 2048));
    if (!kind) return;
    url = String(url || '');
    const inline = !/^https?:/i.test(url);
    // Live playlists are reloaded every few seconds: one report per URL (per content for blob: URLs).
    const key = inline ? kind + text.length + text.slice(0, 300) : url;
    if (seen.has(key) || seen.size > 300) return;
    seen.add(key);
    try {
      document.dispatchEvent(new CustomEvent(EVENT, { detail: JSON.stringify({ url, kind, text: text.length <= MAX ? text : '' }) }));
    } catch { /* not reportable */ }
  }

  // Binary responses: only ones that start like a manifest are decoded.
  const looksTextual = (bytes) => {
    let i = 0;
    if (bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) i = 3;
    while (i < bytes.length && (bytes[i] === 0x20 || bytes[i] === 0x0a || bytes[i] === 0x0d || bytes[i] === 0x09)) i++;
    return bytes[i] === 0x23 /* # */ || bytes[i] === 0x3c /* < */;
  };
  function reportBuffer(url, buf) {
    if (!buf || buf.byteLength > MAX) return;
    const bytes = new Uint8Array(buf, 0, Math.min(buf.byteLength, 64));
    if (looksTextual(bytes)) report(url, new TextDecoder().decode(buf));
  }

  // ---------- XMLHttpRequest (hls.js, video.js, JW Player, Clappr…) ----------
  try {
    const proto = XMLHttpRequest.prototype;
    const open = proto.open, send = proto.send;
    proto.open = function (method, url) {
      try { this.__edmUrl = String(url); } catch {}
      return open.apply(this, arguments);
    };
    proto.send = function () {
      try {
        this.addEventListener('load', function () {
          try {
            const url = this.responseURL || this.__edmUrl;
            const rt = this.responseType;
            if (rt === '' || rt === 'text') report(url, this.responseText);
            else if (rt === 'arraybuffer') reportBuffer(url, this.response);
          } catch {}
        });
      } catch {}
      return send.apply(this, arguments);
    };
  } catch {}

  // ---------- fetch (Shaka, newer hls.js builds…) ----------
  async function sniff(res, url) {
    const reader = res.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!chunks.length && !looksTextual(value.subarray(0, 64))) return;
        chunks.push(value);
        size += value.byteLength;
        if (size > MAX) return;
      }
    } finally {
      reader.cancel().catch(() => {});
    }
    const all = new Uint8Array(size);
    let at = 0;
    for (const c of chunks) { all.set(c, at); at += c.byteLength; }
    report(url, new TextDecoder().decode(all));
  }
  try {
    const origFetch = window.fetch;
    if (typeof origFetch === 'function') {
      const wrapped = function (input) {
        const p = origFetch.apply(this, arguments);
        try {
          p.then((res) => {
            try {
              if (!res || !res.ok || !res.body || res.type === 'opaque') return;
              if (Number(res.headers.get('content-length') || 0) > MAX) return;
              const ct = (res.headers.get('content-type') || '').toLowerCase();
              // Disguised playlists come with any type (text/html, image/png, video/mp2t…); only the first bytes are read.
              if (/^font\/|javascript|css|wasm/.test(ct)) return;
              const url = res.url || (typeof input === 'string' ? input : (input && input.url) || '');
              sniff(res.clone(), url).catch(() => {});
            } catch {}
          }, () => {});
        } catch {}
        return p;
      };
      try { Object.defineProperty(wrapped, 'toString', { value: () => origFetch.toString() }); } catch {}
      window.fetch = wrapped;
    }
  } catch {}
})();
