// Sends what the other people on a call in the browser (Google Meet, and the
// Zoom and Teams web clients) say to Felix (through
// the background worker), so the meeting recording has the call itself and
// not a video playing in another tab. Felix then leaves the browser out of
// the Mac's sound while this comes in.
//
// The audio is what the page plays: the streams on its unmuted <audio> and
// <video> elements. Never the mic, and the user's own (muted) preview is
// skipped. Sent as 16 kHz 16-bit PCM, or just a sample count when silent.
// A client that plays the call another way (decoding it itself) sends
// nothing, and Felix keeps recording the browser's sound as before.

(() => {
  if (window.__felixCallAudio) return;
  window.__felixCallAudio = true;

  const APP = location.hostname === "meet.google.com"
    ? "meet"
    : location.hostname.endsWith("zoom.us")
      ? "zoom"
      : "teams";
  const RATE = 16000;
  // Samples per message: 128 ms.
  const CHUNK = 2048;
  const SCAN_MS = 1000;
  // Quieter than this counts as silence (about -80 dBFS).
  const SILENT = 1e-4;

  let ctx = null;
  let sink = null;
  const sources = new Map(); // track id -> { track, node }
  let stopped = false;

  function send(msg) {
    try {
      chrome.runtime.sendMessage({ app: APP, ...msg });
    } catch (e) {
      // The extension was reloaded; this page's script is orphaned.
      stop();
    }
  }

  function toBase64(samples) {
    const pcm = new Int16Array(samples.length);
    for (let i = 0; i < samples.length; i++) {
      const v = Math.max(-1, Math.min(1, samples[i]));
      pcm[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
    }
    const bytes = new Uint8Array(pcm.buffer);
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x2000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x2000));
    }
    return btoa(binary);
  }

  function open() {
    ctx = new AudioContext({ sampleRate: RATE });
    sink = ctx.createScriptProcessor(CHUNK, 1, 1);
    sink.onaudioprocess = (e) => {
      if (stopped || sources.size === 0) return;
      const x = e.inputBuffer.getChannelData(0);
      let loud = false;
      for (let i = 0; i < x.length; i++) {
        if (Math.abs(x[i]) > SILENT) {
          loud = true;
          break;
        }
      }
      send(
        loud
          ? { type: "call_audio", rate: ctx.sampleRate, pcm: toBase64(x) }
          : { type: "call_audio", rate: ctx.sampleRate, n: x.length },
      );
    };
    // A ScriptProcessor only runs when connected to the output; through a
    // muted gain, so nothing plays twice.
    const mute = ctx.createGain();
    mute.gain.value = 0;
    sink.connect(mute).connect(ctx.destination);
  }

  function scan() {
    if (stopped) return;
    for (const [id, s] of sources) {
      if (s.track.readyState === "ended") {
        s.node.disconnect();
        sources.delete(id);
      }
    }
    for (const el of document.querySelectorAll("audio, video")) {
      const stream = el.srcObject;
      if (el.muted || !(stream instanceof MediaStream)) continue;
      for (const track of stream.getAudioTracks()) {
        if (track.readyState !== "live" || sources.has(track.id)) continue;
        if (!ctx) open();
        const node = ctx.createMediaStreamSource(new MediaStream([track]));
        node.connect(sink);
        sources.set(track.id, { track, node });
      }
    }
    if (ctx && ctx.state === "suspended" && sources.size > 0) ctx.resume().catch(() => {});
  }

  function stop() {
    stopped = true;
    window.__felixCallAudio = false;
    clearInterval(timer);
    for (const s of sources.values()) s.node.disconnect();
    sources.clear();
    if (ctx) ctx.close().catch(() => {});
  }

  const timer = setInterval(scan, SCAN_MS);
  scan();
})();
