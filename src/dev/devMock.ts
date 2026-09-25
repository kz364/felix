// Imported first by main.tsx: in `bun run dev` with `?mock`, stands in for
// the Tauri backend so the window can be previewed in a browser.
import { installTauriMock } from "./mockTauri";

if (
  import.meta.env.DEV &&
  new URLSearchParams(window.location.search).has("mock")
) {
  (window as unknown as Record<string, unknown>).__TAURI_OS_PLUGIN_INTERNALS__ =
    {
      platform: "macos",
      os_type: "macos",
      family: "unix",
      version: "26.0",
      arch: "aarch64",
      exe_extension: "",
      eol: "\n",
    };
  installTauriMock();
}
