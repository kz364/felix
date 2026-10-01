const status = document.getElementById("status");
const captions = document.getElementById("captions");

chrome.runtime.sendMessage({ type: "status" }, (s) => {
  if (!s || !s.host) {
    status.className = "status bad";
    status.textContent = "Felix's helper isn't set up. In Felix: Settings → Meetings → Chrome extension → Set up.";
  } else if (!s.felix) {
    status.className = "status bad";
    status.textContent = "Felix isn't running.";
  } else {
    status.className = "status ok";
    status.textContent = "Connected to Felix.";
  }
});

chrome.storage.local.get({ captions: false }, (v) => (captions.checked = !!v.captions));
captions.addEventListener("change", () => chrome.storage.local.set({ captions: captions.checked }));
