// Watches a call page (Google Meet, and the Zoom and Teams web clients) for
// who's talking and sends it to Felix (through the background worker). Reads names only: no video, and caption
// text only when the user turns captions on in the extension's popup. The
// call's sound is call_audio.js.
//
// Talking is read from the page itself. Each participant's tile animates
// while they talk (Meet's speaking ring and level bars), which shows up as
// a burst of class and style changes inside that tile; video frames don't
// touch the DOM, so the tile with the most changes is the one talking.

(() => {
  const TICK_MS = 500;
  // Changes in a tick for a tile to count as talking.
  const MIN_CHANGES = 2;
  // More than this many tiles busy at once is the layout moving, not talking.
  const MAX_SPEAKING = 2;

  const site = siteAdapter(location.hostname);
  if (!site) return;

  let captionsOn = false;
  chrome.storage.local.get({ captions: false }, (v) => (captionsOn = !!v.captions));
  chrome.storage.onChanged.addListener((c) => {
    if (c.captions) captionsOn = !!c.captions.newValue;
  });

  function send(msg) {
    try {
      chrome.runtime.sendMessage({ app: site.app, ...msg });
    } catch (e) {
      // The extension was reloaded; this page's script is orphaned.
    }
  }

  send({ type: "hello", version: chrome.runtime.getManifest().version });

  // ---- Speaking, from changes inside tiles ----
  const changes = new Map(); // tile element -> count this tick
  const observer = new MutationObserver((records) => {
    for (const r of records) {
      const el = r.target.nodeType === 1 ? r.target : r.target.parentElement;
      if (!el || el.tagName === "VIDEO") continue;
      const tile = el.closest(site.tile);
      if (tile) changes.set(tile, (changes.get(tile) || 0) + 1);
    }
  });
  observer.observe(document.body, {
    subtree: true,
    attributes: true,
    attributeFilter: ["class", "style"],
    childList: true,
  });

  let lastParticipants = "";
  setInterval(() => {
    const busy = [];
    for (const [tile, n] of changes) {
      if (n < MIN_CHANGES || !tile.isConnected || site.isSelf(tile)) continue;
      const name = site.nameOf(tile);
      if (name) busy.push([n, name]);
    }
    changes.clear();
    busy.sort((a, b) => b[0] - a[0]);
    // Pages that say it outright ("Sam Lee, speaking") are believed first.
    const said = site.speaking ? site.speaking() : [];
    const names = [...new Set(said.length ? said : busy.map((b) => b[1]))];
    if (names.length > 0 && names.length <= MAX_SPEAKING) {
      send({ type: "speaking", names });
    }

    const everyone = site.participants();
    const key = everyone.join("\n");
    if (everyone.length && key !== lastParticipants) {
      lastParticipants = key;
      send({ type: "participants", names: everyone });
    }

    if (captionsOn) readCaptions();
  }, TICK_MS);

  // ---- Captions (off unless turned on): send each line once it settles ----
  const pending = new Map(); // element -> {name, text, since}
  const sent = new WeakSet();
  function readCaptions() {
    const now = Date.now();
    for (const line of site.captionLines()) {
      if (sent.has(line.el)) continue;
      const p = pending.get(line.el);
      if (!p || p.text !== line.text) {
        pending.set(line.el, { name: line.name, text: line.text, since: now });
      }
    }
    for (const [el, p] of pending) {
      const gone = !el.isConnected;
      if (gone || now - p.since > 1500) {
        if (p.name && p.text) send({ type: "caption", name: p.name, text: p.text });
        pending.delete(el);
        sent.add(el);
      }
    }
  }

  function clean(name) {
    if (!name) return null;
    const n = name.replace(/\s+/g, " ").trim();
    if (!n || n.length > 80) return null;
    // Icons drawn from words ("frame_person", "keep_outline") aren't names.
    if (/^[a-z0-9_]+$/.test(n)) return null;
    // Nor are notices and labels in a tile ("Others might still see your
    // full video.", "… (visible to everyone)").
    if (/[.!?:]$|[()]/.test(n) || n.split(" ").length > 4) return null;
    if (/^(you|me|presentation|meeting host)$/i.test(n)) return null;
    return n;
  }

  function siteAdapter(host) {
    if (host === "meet.google.com") {
      return {
        app: "meet",
        tile: "[data-participant-id]",
        isSelf: (tile) =>
          !!tile.querySelector("[data-self-name]") ||
          /\(you\)/i.test(tile.getAttribute("aria-label") || ""),
        nameOf: (tile) => meetName(tile),
        participants: () => {
          const names = new Set();
          for (const tile of document.querySelectorAll("[data-participant-id]")) {
            if (tile.parentElement && tile.parentElement.closest("[data-participant-id]")) continue;
            const n = meetName(tile);
            if (n) names.add(n);
          }
          return [...names].sort();
        },
        captionLines: () => {
          const region =
            document.querySelector('[role="region"][aria-label*="aption" i]') ||
            document.querySelector('[jsname="dsyhDe"]');
          if (!region) return [];
          const out = [];
          for (const block of region.children) {
            const nameEl = block.querySelector("img[alt]") ? null : block.querySelector("span, div");
            const name = clean(
              (block.querySelector("img[alt]") && block.querySelector("img[alt]").alt) ||
                (nameEl && nameEl.textContent),
            );
            const text = (block.textContent || "").replace(name || "", "").trim();
            if (name && text) out.push({ el: block, name, text });
          }
          return out;
        },
      };
    }
    if (host === "app.zoom.us" || host.endsWith(".zoom.us")) {
      if (!location.pathname.includes("/wc/")) return null;
      return {
        app: "zoom",
        tile: '[class*="video-frame"], [class*="avatar__avatar"]',
        isSelf: () => false,
        nameOf: (tile) =>
          clean(
            textOf(tile.querySelector('[class*="avatar-name"], [class*="footer"] span')) ||
              tile.getAttribute("aria-label"),
          ),
        speaking: () => {
          const out = labelled();
          for (const el of document.querySelectorAll(
            '[class*="active-speaker"] [class*="avatar-name"], [class*="video-frame--active"] [class*="avatar-name"]',
          )) {
            const n = clean(textOf(el));
            if (n) out.push(n);
          }
          return out;
        },
        participants: () =>
          names('[class*="participants-item__display-name"], [class*="participants-item__name"]'),
        captionLines: () => [],
      };
    }
    if (host === "teams.microsoft.com" || host === "teams.live.com" || host === "teams.cloud.microsoft") {
      return {
        app: "teams",
        tile: '[data-tid*="video-tile"], [data-cid*="participant"], [data-stream-type]',
        isSelf: (tile) => /\(you\)|\(me\)/i.test(tile.getAttribute("aria-label") || ""),
        nameOf: (tile) =>
          clean(
            textOf(tile.querySelector('[data-tid*="display-name"], [data-tid*="name"]')) ||
              (tile.getAttribute("aria-label") || "").split(",")[0],
          ),
        speaking: () => labelled(),
        participants: () => names('[data-tid*="roster"] [data-tid*="name"], [data-tid*="participant-name"]'),
        captionLines: () => {
          const out = [];
          for (const item of document.querySelectorAll('[data-tid="closed-caption-text"]')) {
            const block = item.closest('[data-tid*="caption"], li, div');
            const nameEl = block && block.querySelector('[data-tid="author"], [class*="author"]');
            const name = clean(textOf(nameEl));
            const text = textOf(item).trim();
            if (name && text) out.push({ el: item, name, text });
          }
          return out;
        },
      };
    }
    return null;
  }

  // A Meet tile's name: the first text in it that reads as a name. Meet's
  // icons are text too (and share the name's "notranslate" class).
  function meetName(tile) {
    const self = tile.querySelector("[data-self-name]");
    if (self) return clean(self.textContent);
    for (const el of tile.querySelectorAll(".notranslate, [jsname] span, span, div")) {
      if (el.children.length || el.closest('i, [aria-hidden="true"]')) continue;
      const n = clean(el.textContent);
      if (n) return n;
    }
    return null;
  }

  function textOf(el) {
    return el ? el.textContent || "" : "";
  }

  function names(selector) {
    const out = new Set();
    for (const el of document.querySelectorAll(selector)) {
      const n = clean(textOf(el));
      if (n) out.add(n);
    }
    return [...out].sort();
  }

  // "Sam Lee, speaking" / "Sam Lee is speaking" in an aria-label.
  function labelled() {
    const out = [];
    for (const el of document.querySelectorAll('[aria-label*="speaking" i]')) {
      const label = el.getAttribute("aria-label") || "";
      if (/you are speaking|\(you\)|no one is speaking|not speaking/i.test(label)) continue;
      const m = label.match(/^(.+?)(?:,\s*|\s+is\s+)(?:currently\s+)?speaking/i);
      const n = m && clean(m[1]);
      if (n && !out.includes(n)) out.push(n);
    }
    return out;
  }
})();
