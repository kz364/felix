//! One-click setup for the local cleanup model: installs Ollama (for its
//! bundled llama-server and model registry) and pulls the selected model.
//!
//! Ollama comes from the official macOS zip, extracted with `ditto` (which
//! keeps the bundle's symlinks and signature intact) into /Applications, or
//! ~/Applications when that isn't writable. Models are pulled with the
//! `ollama` CLI's server, started privately on a spare port for the download
//! and stopped afterwards, so nothing is left running.

use crate::local_llm;
use futures_util::StreamExt;
use log::{info, warn};
use serde::Serialize;
use specta::Type;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const OLLAMA_ZIP_URL: &str = "https://ollama.com/download/Ollama-darwin.zip";
const PROGRESS_EVENT: &str = "local-model-install";

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Debug, Clone, Type)]
pub struct LocalModelStatus {
    /// llama-server and the ollama CLI are available.
    pub runtime_installed: bool,
    pub model: String,
    pub model_installed: bool,
    pub installing: bool,
}

/// Emitted while installing.
#[derive(Serialize, Debug, Clone)]
struct InstallProgress {
    /// "ollama", "model", "done" or "error".
    stage: &'static str,
    /// Bytes done and total for the current stage (total 0 when unknown).
    completed: u64,
    total: u64,
    message: String,
}

fn emit(app: &AppHandle, stage: &'static str, completed: u64, total: u64, message: &str) {
    let _ = app.emit(
        PROGRESS_EVENT,
        InstallProgress {
            stage,
            completed,
            total,
            message: message.to_string(),
        },
    );
}

fn selected_model(app: &AppHandle) -> String {
    crate::settings::get_settings(app)
        .post_process_models
        .get(local_llm::LOCAL_PROVIDER_ID)
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| local_llm::LOCAL_DEFAULT_MODEL.to_string())
}

fn runtime_installed() -> bool {
    local_llm::has_server_binary() && local_llm::find_ollama_cli().is_some()
}

#[tauri::command]
#[specta::specta]
pub fn get_local_model_status(app: AppHandle) -> LocalModelStatus {
    let model = selected_model(&app);
    LocalModelStatus {
        runtime_installed: runtime_installed(),
        model_installed: local_llm::resolve_model(&model).is_ok(),
        model,
        installing: INSTALLING.load(Ordering::SeqCst),
    }
}

/// Install whatever is missing (Ollama, then the model), then start the
/// server if the local provider is selected.
#[tauri::command]
#[specta::specta]
pub async fn install_local_model(app: AppHandle) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Already installing".into());
    }
    let result = install(&app).await;
    INSTALLING.store(false, Ordering::SeqCst);
    match &result {
        Ok(()) => {
            emit(&app, "done", 0, 0, "");
            local_llm::sync_with_settings(&crate::settings::get_settings(&app));
        }
        Err(e) => {
            warn!("Local model setup failed: {e}");
            emit(&app, "error", 0, 0, e);
        }
    }
    result
}

async fn install(app: &AppHandle) -> Result<(), String> {
    if !runtime_installed() {
        install_ollama(app).await?;
    }
    let model = selected_model(app);
    if local_llm::resolve_model(&model).is_err() {
        if model.ends_with(".gguf") || model.starts_with('/') {
            return Err(format!("Model file not found: {model}"));
        }
        pull_model(app, &model).await?;
    }
    Ok(())
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".handy-write-test");
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

async fn install_ollama(app: &AppHandle) -> Result<(), String> {
    info!("Downloading Ollama from {OLLAMA_ZIP_URL}");
    emit(app, "ollama", 0, 0, "Downloading Ollama");
    let zip_path = std::env::temp_dir().join("handy-Ollama-darwin.zip");
    let response = reqwest::get(OLLAMA_ZIP_URL)
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't download Ollama: {e}"))?;
    let total = response.content_length().unwrap_or(0);
    let mut file =
        std::fs::File::create(&zip_path).map_err(|e| format!("Couldn't save Ollama: {e}"))?;
    let mut stream = response.bytes_stream();
    let mut completed = 0u64;
    let mut last_emit = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Ollama download interrupted: {e}"))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Couldn't save Ollama: {e}"))?;
        completed += chunk.len() as u64;
        if last_emit.elapsed() > Duration::from_millis(200) {
            emit(app, "ollama", completed, total, "Downloading Ollama");
            last_emit = Instant::now();
        }
    }
    drop(file);

    emit(app, "ollama", total, total, "Installing Ollama");
    let applications = PathBuf::from("/Applications");
    let dest = if writable(&applications) {
        applications
    } else {
        let home = std::env::var_os("HOME").ok_or("No home directory")?;
        let dir = PathBuf::from(home).join("Applications");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {dir:?}: {e}"))?;
        dir
    };
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&zip_path)
        .arg(&dest)
        .status()
        .map_err(|e| format!("Couldn't unpack Ollama: {e}"))?;
    let _ = std::fs::remove_file(&zip_path);
    if !status.success() {
        return Err("Couldn't unpack Ollama".into());
    }
    if !runtime_installed() {
        return Err("Ollama was installed but its llama-server wasn't found".into());
    }
    info!("Installed Ollama into {}", dest.display());
    Ok(())
}

/// Stops the private `ollama serve` when the pull ends, however it ends.
struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn pull_model(app: &AppHandle, model: &str) -> Result<(), String> {
    let cli = local_llm::find_ollama_cli().ok_or("The ollama command wasn't found")?;
    let port = local_llm::free_port().ok_or("No free local port")?;
    let host = format!("127.0.0.1:{port}");
    info!("Pulling {model} with {}", cli.display());
    emit(app, "model", 0, 0, &format!("Starting download of {model}"));
    let _serve = ServeGuard(
        Command::new(&cli)
            .arg("serve")
            .env("OLLAMA_HOST", &host)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Couldn't start Ollama: {e}"))?,
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while client
        .get(format!("http://{host}/api/version"))
        .send()
        .await
        .is_err()
    {
        if Instant::now() > deadline {
            return Err("Ollama didn't start".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let response = client
        .post(format!("http://{host}/api/pull"))
        .json(&serde_json::json!({"model": model, "stream": true}))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't download {model}: {e}"))?;
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut last_emit = Instant::now();
    let mut succeeded = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download of {model} interrupted: {e}"))?;
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let Ok(update) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(error) = update["error"].as_str() {
                return Err(format!("Couldn't download {model}: {error}"));
            }
            let status = update["status"].as_str().unwrap_or_default();
            if status == "success" {
                succeeded = true;
            }
            let total = update["total"].as_u64().unwrap_or(0);
            // Status-only updates (no byte counts) always go through.
            if last_emit.elapsed() > Duration::from_millis(200) || total == 0 {
                let completed = update["completed"].as_u64().unwrap_or(0);
                emit(
                    app,
                    "model",
                    completed,
                    total,
                    &format!("Downloading {model}"),
                );
                last_emit = Instant::now();
            }
        }
    }
    if !succeeded || local_llm::resolve_model(model).is_err() {
        return Err(format!("Download of {model} didn't finish"));
    }
    info!("Pulled {model}");
    Ok(())
}
