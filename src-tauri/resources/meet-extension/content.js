// Watches a call page for who's talking and sends it to Felix (through the
// background worker). Reads names only: no audio, no video, and caption
// text only when the user turns captions on in the extension's popup.
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
    const names = [...new Set(busy.map((b) => b[1]))];
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
        nameOf: (tile) => {
          const el =
            tile.querySelector("[data-self-name]") ||
            tile.querySelector(".notranslate") ||
            tile.querySelector("[jsname] span");
          return clean(el && el.textContent);
        },
        participants: () => {
          const names = new Set();
          for (const tile of document.querySelectorAll("[data-participant-id]")) {
            if (tile.parentElement && tile.parentElement.closest("[data-participant-id]")) continue;
            const el = tile.querySelector(".notranslate");
            const n = clean(el && el.textContent);
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
    return null;
  }
})();
