// Relays what the call pages see to Felix through its native host
// (com.felix.meetings). Felix logs it only while a meeting records.

const HOST = "com.felix.meetings";
let port = null;
let felix = false;

function connect() {
  if (port) return port;
  try {
    port = chrome.runtime.connectNative(HOST);
  } catch (e) {
    port = null;
    felix = false;
    return null;
  }
  port.onMessage.addListener((reply) => {
    if (reply && typeof reply.felix === "boolean") felix = reply.felix;
  });
  port.onDisconnect.addListener(() => {
    // chrome.runtime.lastError is read so Chrome doesn't log it as unchecked.
    void chrome.runtime.lastError;
    port = null;
    felix = false;
  });
  port.postMessage({
    type: "hello",
    app: "extension",
    version: chrome.runtime.getManifest().version,
  });
  return port;
}

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg && msg.type === "status") {
    // The popup asks; say hello again so the answer is fresh.
    const p = connect();
    if (p) {
      p.postMessage({
        type: "hello",
        app: "extension",
        version: chrome.runtime.getManifest().version,
      });
    }
    setTimeout(() => reply({ host: !!port, felix }), 300);
    return true;
  }
  const p = connect();
  if (p && msg && typeof msg.type === "string") p.postMessage(msg);
  return false;
});

// ---- Keeping up with Felix's copy ----
// Loaded unpacked from Felix's folder, the extension picks up a newer copy
// by itself: now and then it reads its own manifest from disk and reloads
// when the version changed, then starts itself again on open call pages.
const CHECK_EVERY_MS = 60 * 1000;
let checkedAt = 0;

async function checkForUpdate() {
  if (Date.now() - checkedAt < CHECK_EVERY_MS) return;
  checkedAt = Date.now();
  try {
    const r = await fetch(chrome.runtime.getURL("manifest.json"), { cache: "no-store" });
    const onDisk = (await r.json()).version;
    if (onDisk && onDisk !== chrome.runtime.getManifest().version) chrome.runtime.reload();
  } catch (e) {
    // Not readable right now; try again later.
  }
}

chrome.runtime.onMessage.addListener(() => {
  checkForUpdate();
  return false;
});
chrome.runtime.onStartup.addListener(checkForUpdate);

chrome.runtime.onInstalled.addListener(async () => {
  for (const script of chrome.runtime.getManifest().content_scripts) {
    for (const tab of await chrome.tabs.query({ url: script.matches })) {
      chrome.scripting
        .executeScript({ target: { tabId: tab.id }, files: script.js })
        .catch(() => {});
    }
  }
});
