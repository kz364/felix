// Sends what the other people on a call in the browser (Google Meet, and the
// Zoom and Teams web clients) say to Felix, so the meeting recording has the
// call itself and not a video playing in another tab. Felix then leaves the
// browser out of the Mac's sound while this comes in.
//
// Runs in the page's own world from the start, so it sees the call's
// connections as the page makes them: every remote audio track that arrives
// is mixed in. Never the mic (that's a local track). For a page that was
// already on a call when this was added, the unmuted <audio>/<video>
// elements playing a stream are picked up instead. Chunks go to content.js
// by window.postMessage (this world has no extension APIs), which passes
// them to Felix: 16 kHz 16-bit PCM, or just a sample count when silent.
// Every STATUS_MS it also says what it found, for Felix's log.

(() => {
  if (window.__felixCallAudio) return;
  window.__felixCallAudio = true;

  const RATE = 16000;
  // Samples per message: 128 ms.
  const CHUNK = 2048;
  const SCAN_MS = 1000;
  const STATUS_MS = 10000;
  // Quieter than this counts as silence (about -80 dBFS).
  const SILENT = 1e-4;

  let ctx = null;
  let sink = null;
  const sources = new Map(); // track id -> { track, node }
  let peers = 0;

  function post(msg) {
    window.postMessage({ __felixCallAudio: true, msg }, location.origin);
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
      if (sources.size === 0) return;
      const x = e.inputBuffer.getChannelData(0);
      let loud = false;
      for (let i = 0; i < x.length; i++) {
        if (Math.abs(x[i]) > SILENT) {
          loud = true;
          break;
        }
      }
      post(
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

  function add(track) {
    if (track.kind !== "audio" || track.readyState !== "live") return;
    if (sources.has(track.id)) return;
    if (!ctx) open();
    const node = ctx.createMediaStreamSource(new MediaStream([track]));
    node.connect(sink);
    sources.set(track.id, { track, node });
  }

  // The call's connections: each remote track as it arrives.
  const Native = window.RTCPeerConnection;
  if (Native) {
    class Watched extends Native {
      constructor(...args) {
        super(...args);
        peers++;
        this.addEventListener("track", (e) => add(e.track));
      }
    }
    window.RTCPeerConnection = Watched;
    if (window.webkitRTCPeerConnection) window.webkitRTCPeerConnection = Watched;
  }

  function scan() {
    for (const [id, s] of sources) {
      if (s.track.readyState === "ended") {
        s.node.disconnect();
        sources.delete(id);
      }
    }
    for (const el of document.querySelectorAll("audio, video")) {
      const stream = el.srcObject;
      if (el.muted || !(stream instanceof MediaStream)) continue;
      for (const track of stream.getAudioTracks()) add(track);
    }
    if (ctx && ctx.state === "suspended" && sources.size > 0) ctx.resume().catch(() => {});
  }

  setInterval(scan, SCAN_MS);
  setInterval(() => {
    if (peers === 0 && sources.size === 0) return;
    post({
      type: "call_audio_status",
      connections: peers,
      tracks: sources.size,
      state: ctx ? ctx.state : "none",
    });
  }, STATUS_MS);
})();
