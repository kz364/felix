//! On-device cleanup with a small open model (Qwen3.5 4B by default) served
//! by a llama-server process that Handy starts and owns.
//!
//! Why not the "Custom" OpenAI-compatible provider pointed at Ollama: the
//! latency win comes from caching the prompt up to the transcript while the
//! user is still speaking (system prompt, instructions, vocabulary), so only
//! the transcript itself is processed after they stop. That needs
//! llama-server's raw `/completion` endpoint with `cache_prompt`. Hybrid
//! attention models like Qwen3.5 can't roll their cache back, so the cached
//! prefix must be an exact token prefix of the real request; both are
//! rendered with the model's own chat template via `/apply-template`.
//!
//! Models are Ollama's (`qwen3.5:4b` resolves to the GGUF blob under
//! `~/.ollama/models`), or an absolute path to a `.gguf` file. The binary is
//! llama.cpp's `llama-server` (Homebrew `llama.cpp`, or the copy bundled with
//! Ollama). Ollama itself doesn't need to be running.

use log::{debug, info, warn};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const LOCAL_PROVIDER_ID: &str = "local";
pub const LOCAL_DEFAULT_MODEL: &str = "qwen3.5:4b";
/// For Macs with less memory: 1.9 GB instead of 3.4 GB, a bit less careful.
pub const LOCAL_LIGHT_MODEL: &str = "qwen3.5:2b-q4_K_M";

const CONTEXT_TOKENS: &str = "4096";
const MAX_OUTPUT_TOKENS: u32 = 1024;
/// A cold start maps a few GB of weights and compiles Metal kernels.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// On-demand mode: how long the model stays loaded after a cleanup, and
/// after a prewarm that no cleanup followed (e.g. a long dictation, or one
/// that needed no AI cleanup).
const IDLE_UNLOAD: Duration = Duration::from_secs(30);
const PREWARM_UNLOAD: Duration = Duration::from_secs(600);
/// Stands in for the transcript when rendering the prefix to cache.
const MARKER: &str = "\u{2063}HANDY_TRANSCRIPT\u{2063}";

struct Server {
    child: Child,
    model: String,
    port: u16,
}

static SERVER: Lazy<Mutex<Option<Server>>> = Lazy::new(|| Mutex::new(None));
/// Bumped on every use; a scheduled unload only fires if nothing used the
/// server since it was scheduled.
static USE_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Serializes starts so a prewarm and a request don't both spawn a server.
static STARTING: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

fn http() -> &'static reqwest::Client {
    static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("reqwest client")
    });
    &CLIENT
}

fn ollama_models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("OLLAMA_MODELS") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ollama/models"))
}

/// Path of the GGUF weights for an Ollama tag like `qwen3.5:4b` (or a path).
pub fn resolve_model(model: &str) -> Result<PathBuf, String> {
    let model = model.trim();
    if model.ends_with(".gguf") || model.starts_with('/') {
        let path = PathBuf::from(model);
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| format!("Model file not found: {model}"));
    }
    let dir = ollama_models_dir().ok_or("No home directory")?;
    let (name, tag) = model.split_once(':').unwrap_or((model, "latest"));
    let manifest_path = if name.contains('/') {
        dir.join("manifests/registry.ollama.ai")
            .join(name)
            .join(tag)
    } else {
        dir.join("manifests/registry.ollama.ai/library")
            .join(name)
            .join(tag)
    };
    let manifest: Value = std::fs::read(&manifest_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| format!("Model '{model}' isn't installed; install it from Settings → Post Processing (or run: ollama pull {model})"))?;
    let digest = manifest["layers"]
        .as_array()
        .and_then(|layers| {
            layers.iter().find(|l| {
                l["mediaType"]
                    .as_str()
                    .is_some_and(|t| t.ends_with(".model"))
            })
        })
        .and_then(|l| l["digest"].as_str())
        .ok_or_else(|| format!("No weights in the manifest for '{model}'"))?;
    let blob = dir.join("blobs").join(digest.replace(':', "-"));
    blob.is_file()
        .then_some(blob)
        .ok_or_else(|| format!("Weights for '{model}' are missing; re-run: ollama pull {model}"))
}

/// The recommended models (installable from settings) plus any other
/// installed Ollama models, for the model dropdown.
pub fn available_models() -> Vec<String> {
    let mut models = vec![
        LOCAL_DEFAULT_MODEL.to_string(),
        LOCAL_LIGHT_MODEL.to_string(),
    ];
    for model in installed_models() {
        if !models.contains(&model) {
            models.push(model);
        }
    }
    models
}

fn installed_models() -> Vec<String> {
    let Some(root) = ollama_models_dir().map(|d| d.join("manifests/registry.ollama.ai")) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    let Ok(namespaces) = std::fs::read_dir(&root) else {
        return models;
    };
    for ns in namespaces.flatten() {
        let ns_name = ns.file_name().to_string_lossy().to_string();
        let Ok(names) = std::fs::read_dir(ns.path()) else {
            continue;
        };
        for name in names.flatten() {
            let Ok(tags) = std::fs::read_dir(name.path()) else {
                continue;
            };
            for tag in tags.flatten() {
                let name = name.file_name().to_string_lossy().to_string();
                let tag = tag.file_name().to_string_lossy().to_string();
                let full = if ns_name == "library" {
                    format!("{name}:{tag}")
                } else {
                    format!("{ns_name}/{name}:{tag}")
                };
                if resolve_model(&full).is_ok() {
                    models.push(full);
                }
            }
        }
    }
    models.sort();
    models
}

/// Ollama.app bundles (where the installer puts it, or a manual install).
pub(crate) fn ollama_app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications/Ollama.app")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications/Ollama.app"));
    }
    dirs
}

/// Locations of an executable: the PATH, Homebrew (GUI apps don't get the
/// shell's PATH), then Ollama.app's Resources.
fn find_executable(name: &str) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join(name)).collect())
        .unwrap_or_default();
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(name));
    candidates.push(PathBuf::from("/usr/local/bin").join(name));
    for app in ollama_app_dirs() {
        candidates.push(app.join("Contents/Resources").join(name));
    }
    candidates
}

fn find_server_binary() -> Option<PathBuf> {
    let mut candidates = find_executable("llama-server");
    // Homebrew's Ollama bundles one (newest version first).
    for cellar in ["/opt/homebrew/Cellar/ollama", "/usr/local/Cellar/ollama"] {
        if let Ok(entries) = std::fs::read_dir(cellar) {
            let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            versions.sort();
            for v in versions.into_iter().rev() {
                candidates.push(v.join("libexec/lib/ollama/llama-server"));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub(crate) fn find_ollama_cli() -> Option<PathBuf> {
    find_executable("ollama").into_iter().find(|p| p.is_file())
}

pub(crate) fn has_server_binary() -> bool {
    find_server_binary().is_some()
}

pub(crate) fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

/// The server's own output, for diagnosing load failures.
fn log_file() -> PathBuf {
    std::env::temp_dir().join("handy-llama-server.log")
}

fn pid_file() -> PathBuf {
    std::env::temp_dir().join("handy-llama-server.pid")
}

/// A server left behind by a crashed Handy would hold GBs of memory.
fn kill_stale_server() {
    let Some(pid) = std::fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return;
    };
    let is_llama = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("llama-server"));
    if is_llama {
        info!("Stopping a leftover llama-server (pid {pid})");
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    let _ = std::fs::remove_file(pid_file());
}

fn spawn(model: &str) -> Result<Server, String> {
    let weights = resolve_model(model)?;
    let binary = find_server_binary()
        .ok_or("llama-server not found. Install Ollama from Settings → Post Processing")?;
    let port = free_port().ok_or("No free local port")?;
    kill_stale_server();
    let stderr = std::fs::File::create(log_file())
        .ok()
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    let child = Command::new(&binary)
        .arg("-m")
        .arg(&weights)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["-c", CONTEXT_TOKENS, "-np", "1", "--jinja"])
        .args(["--reasoning", "off", "--flash-attn", "on", "-ngl", "99"])
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {e}", binary.display()))?;
    let _ = std::fs::write(pid_file(), child.id().to_string());
    info!(
        "Started llama-server (pid {}) on port {port} with {model} from {}",
        child.id(),
        binary.display()
    );
    Ok(Server {
        child,
        model: model.to_string(),
        port,
    })
}

/// Stop the server (app exit, or switching away from the local provider).
pub fn stop() {
    if let Some(mut server) = SERVER.lock().unwrap().take() {
        let _ = server.child.kill();
        let _ = server.child.wait();
        let _ = std::fs::remove_file(pid_file());
        debug!("Stopped llama-server");
    }
}

/// The port of a running, healthy server for `model`, starting (or
/// restarting, when the model changed) it as needed.
async fn ensure_running(model: &str) -> Result<u16, String> {
    let _guard = STARTING.lock().await;
    let existing = SERVER.lock().unwrap().as_mut().and_then(|s| {
        (s.model == model && matches!(s.child.try_wait(), Ok(None))).then_some(s.port)
    });
    if existing.is_none() {
        // Wrong model, or the process died.
        stop();
    }
    let port = match existing {
        Some(port) => port,
        None => {
            let server = spawn(model)?;
            let port = server.port;
            *SERVER.lock().unwrap() = Some(server);
            port
        }
    };
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let healthy = http()
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if healthy {
            return Ok(port);
        }
        let exited = SERVER
            .lock()
            .unwrap()
            .as_mut()
            .is_none_or(|s| !matches!(s.child.try_wait(), Ok(None)));
        if exited {
            stop();
            return Err("llama-server exited while loading the model (see its log in the temp folder: handy-llama-server.log)".into());
        }
        if Instant::now() > deadline {
            return Err("llama-server didn't become ready in time".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn post(port: u16, path: &str, body: Value) -> Result<Value, String> {
    let response = http()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("llama-server {path}: {e}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("llama-server {path}: {e}"))?;
    if !status.is_success() {
        return Err(format!("llama-server {path} returned {status}: {value}"));
    }
    Ok(value)
}

/// The model's chat template applied to a system + user turn, ending with
/// the assistant header (thinking off).
async fn render(port: u16, system: &str, user: &str) -> Result<String, String> {
    let value = post(
        port,
        "/apply-template",
        json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "chat_template_kwargs": {"enable_thinking": false},
        }),
    )
    .await?;
    value["prompt"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "llama-server /apply-template returned no prompt".into())
}

/// Keep the server running only while it's the cleanup provider: start it
/// ahead of time (so the first dictation doesn't pay for loading the model),
/// and free its memory when another provider is chosen or cleanup is off.
pub fn sync_with_settings(settings: &crate::settings::AppSettings) {
    let model = settings
        .post_process_models
        .get(LOCAL_PROVIDER_ID)
        .cloned()
        .unwrap_or_default();
    let selected = settings.post_process_enabled
        && settings.post_process_provider_id == LOCAL_PROVIDER_ID
        && !model.trim().is_empty();
    if selected && settings.local_model_keep_loaded {
        warm_up(model);
    } else if !selected || SERVER.lock().unwrap().is_some() {
        // Not the provider, or just switched to on-demand: free the memory
        // now; the next dictation loads it again.
        stop();
    }
}

/// Mark the server as in use, cancelling any pending idle unload.
fn touch() -> u64 {
    USE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// On-demand mode: stop the server after `idle` unless it's used again.
fn unload_when_idle(idle: Duration) {
    let generation = touch();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(idle).await;
        if USE_GENERATION.load(Ordering::SeqCst) == generation {
            debug!("Unloading the idle local cleanup model");
            stop();
        }
    });
}

fn warm_up(model: String) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = ensure_running(&model).await {
            warn!("Local cleanup model unavailable: {e}");
        }
    });
}

/// Process the prompt up to where the transcript goes, so the request after
/// the user stops talking only has to process the transcript. `user_prefix`
/// is the user-turn text that precedes the transcript.
/// In on-demand mode (`keep_loaded` off) this is also what loads the model,
/// while the user speaks.
pub fn prewarm(model: String, system_prompt: String, user_prefix: String, keep_loaded: bool) {
    touch();
    tauri::async_runtime::spawn(async move {
        let started = Instant::now();
        match prewarm_now(&model, &system_prompt, &user_prefix).await {
            Ok(()) => debug!("Prewarmed local cleanup model in {:?}", started.elapsed()),
            Err(e) => warn!("Prewarming the local cleanup model failed: {e}"),
        }
        if !keep_loaded {
            unload_when_idle(PREWARM_UNLOAD);
        }
    });
}

async fn prewarm_now(model: &str, system_prompt: &str, user_prefix: &str) -> Result<(), String> {
    let port = ensure_running(model).await?;
    let rendered = render(port, system_prompt, &format!("{user_prefix}{MARKER}")).await?;
    let head = rendered
        .split(MARKER)
        .next()
        .ok_or("marker missing from the rendered prompt")?;
    post(
        port,
        "/completion",
        json!({"prompt": head, "n_predict": 0, "cache_prompt": true}),
    )
    .await
    .map(|_| ())
}

/// Run one cleanup request. Greedy decoding, reusing whatever prefix of the
/// prompt is already cached.
pub async fn complete(
    model: &str,
    system_prompt: &str,
    user: &str,
    keep_loaded: bool,
) -> Result<String, String> {
    touch();
    let result = complete_now(model, system_prompt, user).await;
    if !keep_loaded {
        unload_when_idle(IDLE_UNLOAD);
    }
    result
}

async fn complete_now(model: &str, system_prompt: &str, user: &str) -> Result<String, String> {
    let started = Instant::now();
    let port = ensure_running(model).await?;
    let prompt = render(port, system_prompt, user).await?;
    let value = post(
        port,
        "/completion",
        json!({
            "prompt": prompt,
            "n_predict": MAX_OUTPUT_TOKENS,
            "temperature": 0,
            "cache_prompt": true,
        }),
    )
    .await?;
    let cached = value["tokens_cached"].as_u64().unwrap_or(0);
    let evaluated = value["timings"]["prompt_n"].as_u64().unwrap_or(0);
    debug!(
        "Local cleanup took {:?} ({evaluated} new prompt tokens, {cached} cached)",
        started.elapsed()
    );
    value["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "llama-server returned no content".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_explain_how_to_install() {
        let err = resolve_model("definitely-not-a-model:9b").unwrap_err();
        assert!(err.contains("ollama pull"), "{err}");
        assert!(resolve_model("/nonexistent/model.gguf").is_err());
    }

    /// Needs llama-server and `ollama pull qwen3.5:4b`:
    /// cargo test --lib local_llm -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn prewarmed_cleanup_reuses_the_prefix() {
        let system = "You clean up dictated text. Fix punctuation and capitalization, remove filler words. Output only the cleaned text.";
        let prefix = crate::cleanup::CLEANUP_USER_PREFIX;
        prewarm_now(LOCAL_DEFAULT_MODEL, system, prefix)
            .await
            .unwrap();
        let user = format!(
            "{prefix}{}",
            crate::cleanup::wrap_transcript("um so i think we should uh ship it on friday")
        );
        let started = Instant::now();
        let out = complete_now(LOCAL_DEFAULT_MODEL, system, &user)
            .await
            .unwrap();
        println!("{:?} -> {out}", started.elapsed());
        assert!(out.to_lowercase().contains("friday"), "{out}");
        assert!(!out.contains("um "), "{out}");
        stop();
    }
}
