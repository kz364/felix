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
