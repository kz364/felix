//! Where the dictation is going: frontmost app (name + bundle id) and, in
//! supported browsers, the website of the active tab. Captured when recording
//! starts, because Handy's own overlay may be frontmost by the time the text
//! is processed. The browser URL is read in the background (AppleScript, one
//! macOS Automation prompt per browser) so it never delays recording.

use crate::settings::{AppCategory, AppRule, AppRuleKind, AppSettings};
use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DictationContext {
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    /// Host of the active browser tab ("mail.google.com"), when known.
    pub url_host: Option<String>,
    /// The app's declared App Store category (`LSApplicationCategoryType`).
    pub declared_category: Option<String>,
}

static CURRENT: Lazy<Arc<Mutex<DictationContext>>> =
    Lazy::new(|| Arc::new(Mutex::new(DictationContext::default())));

/// Chromium-family browsers share Chrome's AppleScript dictionary.
const CHROMIUM_BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",
    "com.vivaldi.Vivaldi",
];
const SAFARI_BROWSERS: &[&str] = &["com.apple.Safari", "com.apple.SafariTechnologyPreview"];

pub fn is_browser(bundle_id: &str) -> bool {
    CHROMIUM_BROWSERS.contains(&bundle_id) || SAFARI_BROWSERS.contains(&bundle_id)
}

/// Chromium-based apps where Handy doesn't switch the Accessibility tree on:
/// VS Code and its forks take it as a screen reader and change the editor.
const NO_TREE_WAKE: &[&str] = &[
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.vscodium",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.exafunction.windsurf",
];

/// Whether the app is Chromium or Electron (they build their Accessibility
/// tree only once asked, through `AXManualAccessibility`).
fn is_chromium_based(bundle_id: Option<&str>, bundle_path: Option<&str>) -> bool {
    bundle_id.is_some_and(|b| CHROMIUM_BROWSERS.contains(&b))
        || bundle_path.is_some_and(bundles_chromium)
}

/// Whether an app bundle ships Chromium: an Electron framework, including
/// renamed ones like the Codex app's "Codex Framework".
fn bundles_chromium(bundle_path: &str) -> bool {
    let frameworks = std::path::Path::new(bundle_path).join("Contents/Frameworks");
    if frameworks.join("Electron Framework.framework").exists() {
        return true;
    }
    std::fs::read_dir(&frameworks).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with(" Framework.framework")
                && e.path().join("Resources/chrome_100_percent.pak").exists()
        })
    })
}

/// Whether Handy switches this app's Accessibility tree on.
fn wakes_tree(bundle_id: Option<&str>, bundle_path: Option<&str>) -> bool {
    is_chromium_based(bundle_id, bundle_path)
        && !bundle_id.is_some_and(|b| NO_TREE_WAKE.contains(&b) || b == "com.pais.handy")
}

/// The frontmost app's pid, if it's one whose tree Handy switches on.
pub fn frontmost_tree_pid() -> Option<i32> {
    let (_, bundle_id, path) = frontmost_app();
    wakes_tree(bundle_id.as_deref(), path.as_deref())
        .then(frontmost_pid)
        .flatten()
}

/// Record the frontmost app now and start reading the browser URL.
pub fn capture() {
    let (app_name, bundle_id, bundle_path) = frontmost_app();
    // Chromium and Electron only say what's focused once their tree is on.
    // Switch it on now, so by the time the text is ready "nothing focused"
    // can be trusted and the text shown instead of pasted into nothing.
    if wakes_tree(bundle_id.as_deref(), bundle_path.as_deref()) {
        if let Some(pid) = frontmost_pid() {
            std::thread::spawn(move || crate::text_field::wake_tree(pid));
        }
    }
    let ctx = DictationContext {
        app_name,
        bundle_id: bundle_id.clone(),
        url_host: None,
        declared_category: bundle_path
            .as_deref()
            .and_then(crate::app_categories::declared_category_at),
    };
    log::debug!("Dictation context: {:?}", ctx);
    *CURRENT.lock().unwrap() = ctx;

    if let Some(bundle_id) = bundle_id.filter(|b| is_browser(b)) {
        let current = Arc::clone(&CURRENT);
        std::thread::spawn(move || {
            let host = browser_url(&bundle_id).and_then(|url| url_host(&url));
            log::debug!("Browser tab host: {:?}", host);
            let mut ctx = current.lock().unwrap();
            // Only fill in if no newer capture replaced this one.
            if ctx.bundle_id.as_deref() == Some(bundle_id.as_str()) {
                ctx.url_host = host;
            }
        });
    }
}

pub fn current() -> DictationContext {
    CURRENT.lock().unwrap().clone()
}

/// Bundle id of the frontmost app, and whether it builds its Accessibility
/// tree lazily (Chromium browsers, Electron apps, Firefox).
pub fn frontmost_bundle() -> (Option<String>, bool) {
    let (_, bundle_id, path) = frontmost_app();
    let lazy_ax = bundle_id
        .as_deref()
        .is_some_and(|b| b.starts_with("org.mozilla."))
        || is_chromium_based(bundle_id.as_deref(), path.as_deref());
    (bundle_id, lazy_ax)
}

/// Process id of the frontmost app.
#[cfg(target_os = "macos")]
pub fn frontmost_pid() -> Option<i32> {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_pid() -> Option<i32> {
    None
}

/// Name, bundle id and bundle path of the frontmost app.
#[cfg(target_os = "macos")]
fn frontmost_app() -> (Option<String>, Option<String>, Option<String>) {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    match workspace.frontmostApplication() {
        Some(app) => (
            app.localizedName().map(|s| s.to_string()),
            app.bundleIdentifier().map(|s| s.to_string()),
            app.bundleURL()
                .and_then(|url| url.path())
                .map(|p| p.to_string()),
        ),
        None => (None, None, None),
    }
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app() -> (Option<String>, Option<String>, Option<String>) {
    (None, None, None)
}

#[cfg(target_os = "macos")]
fn browser_url(bundle_id: &str) -> Option<String> {
    let script = if SAFARI_BROWSERS.contains(&bundle_id) {
        format!("tell application id \"{bundle_id}\" to get URL of front document")
    } else {
        format!("tell application id \"{bundle_id}\" to get URL of active tab of front window")
    };
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        log::debug!(
            "Browser URL lookup failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty() && url != "missing value").then_some(url)
}

#[cfg(not(target_os = "macos"))]
fn browser_url(_bundle_id: &str) -> Option<String> {
    None
}

/// "https://www.mail.google.com:443/x" → "mail.google.com".
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim()
        .to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    (!host.is_empty() && host.contains('.')).then_some(host)
}

use crate::app_categories::host_matches;

/// Where a context's category came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategorySource {
    Override,
    Automatic,
}

/// Category for a context: the user's overrides (website, then app) win,
/// then automatic categorization (known website, then the app).
pub fn resolve(ctx: &DictationContext, overrides: &[AppRule]) -> (AppCategory, CategorySource) {
    if let Some(host) = &ctx.url_host {
        // Most specific domain first ("mail.google.com" over "google.com").
        if let Some(rule) = overrides
            .iter()
            .filter(|r| r.kind == AppRuleKind::Website && host_matches(host, &r.key))
            .max_by_key(|r| r.key.len())
        {
            return (rule.category, CategorySource::Override);
        }
    }
    if let Some(bundle_id) = &ctx.bundle_id {
        if let Some(rule) = overrides
            .iter()
            .find(|r| r.kind == AppRuleKind::App && r.key == *bundle_id)
        {
            return (rule.category, CategorySource::Override);
        }
    }
    if let Some(category) = ctx
        .url_host
        .as_deref()
        .and_then(crate::app_categories::auto_website_category)
    {
        return (category, CategorySource::Automatic);
    }
    let category = ctx.bundle_id.as_deref().map_or(AppCategory::Other, |id| {
        crate::app_categories::auto_app_category(
            id,
            ctx.app_name.as_deref().unwrap_or_default(),
            ctx.declared_category.as_deref(),
        )
    });
    (category, CategorySource::Automatic)
}

/// The entry the Style page shows for a context: the website when in a
/// browser tab, otherwise the app. `category` is the automatic one.
pub fn context_entry(ctx: &DictationContext) -> Option<AppRule> {
    if let Some(host) = &ctx.url_host {
        return Some(AppRule {
            kind: AppRuleKind::Website,
            key: host.clone(),
            label: host.clone(),
            category: crate::app_categories::auto_website_category(host)
                .unwrap_or(AppCategory::Other),
        });
    }
    let bundle_id = ctx.bundle_id.clone()?;
    if bundle_id == "com.pais.handy" {
        return None;
    }
    let label = ctx.app_name.clone().unwrap_or_else(|| bundle_id.clone());
    Some(AppRule {
        kind: AppRuleKind::App,
        category: crate::app_categories::auto_app_category(
            &bundle_id,
            &label,
            ctx.declared_category.as_deref(),
        ),
        label,
        key: bundle_id,
    })
}

/// Remember a recently dictated-into app or site (newest first, max 12) so
/// the Style page can show it. Returns true when settings need saving.
pub fn remember_recent(settings: &mut AppSettings, ctx: &DictationContext) -> bool {
    let Some(entry) = context_entry(ctx) else {
        return false;
    };
    if settings.recent_contexts.first().map(|r| (&r.kind, &r.key))
        == Some((&entry.kind, &entry.key))
    {
        return false;
    }
    settings
        .recent_contexts
        .retain(|r| !(r.kind == entry.kind && r.key == entry.key));
    settings.recent_contexts.insert(0, entry);
    settings.recent_contexts.truncate(12);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn electron_apps_are_found_even_with_a_renamed_framework() {
        let root = std::env::temp_dir().join(format!("handy-bundles-{}", std::process::id()));
        let app = |name: &str, framework: &str, pak: bool| {
            let res = root
                .join(name)
                .join("Contents/Frameworks")
                .join(framework)
                .join("Resources");
            std::fs::create_dir_all(&res).unwrap();
            if pak {
                std::fs::write(res.join("chrome_100_percent.pak"), b"").unwrap();
            }
            root.join(name).to_string_lossy().to_string()
        };
        // The Codex app ships Electron as "Codex Framework".
        assert!(bundles_chromium(&app(
            "Codex.app",
            "Codex Framework.framework",
            true
        )));
        assert!(bundles_chromium(&app(
            "Claude.app",
            "Electron Framework.framework",
            false
        )));
        assert!(!bundles_chromium(&app(
            "Native.app",
            "Sparkle.framework",
            false
        )));
        assert!(!bundles_chromium(&app(
            "Other.app",
            "Media Framework.framework",
            false
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn ctx(bundle: &str, host: Option<&str>) -> DictationContext {
        DictationContext {
            app_name: Some("App".into()),
            bundle_id: Some(bundle.into()),
            url_host: host.map(str::to_string),
            declared_category: None,
        }
    }

    fn rule(kind: AppRuleKind, key: &str, category: AppCategory) -> AppRule {
        AppRule {
            kind,
            key: key.into(),
            label: key.into(),
            category,
        }
    }

    #[test]
    fn url_hosts() {
        assert_eq!(
            url_host("https://www.mail.google.com/mail/u/0/#inbox").as_deref(),
            Some("mail.google.com")
        );
        assert_eq!(url_host("http://localhost:3000/").as_deref(), None);
        assert_eq!(
            url_host("https://user@app.slack.com:443/client").as_deref(),
            Some("app.slack.com")
        );
        assert_eq!(url_host("about:blank"), None);
    }

    #[test]
    fn automatic_categories() {
        let none: Vec<AppRule> = vec![];
        assert_eq!(
            resolve(&ctx("com.google.Chrome", Some("mail.google.com")), &none).0,
            AppCategory::Email
        );
        assert_eq!(
            resolve(&ctx("com.google.Chrome", Some("docs.google.com")), &none).0,
            AppCategory::Other
        );
        assert_eq!(
            resolve(&ctx("com.tinyspeck.slackmacgap", None), &none).0,
            AppCategory::Work
        );
        assert_eq!(
            resolve(&ctx("com.apple.MobileSMS", None), &none).0,
            AppCategory::Personal
        );
        let mut declared = ctx("com.example.social", None);
        declared.declared_category = Some("public.app-category.social-networking".into());
        assert_eq!(resolve(&declared, &none).0, AppCategory::Personal);
    }

    #[test]
    fn overrides_win_over_automatic() {
        let overrides = vec![
            rule(
                AppRuleKind::App,
                "com.tinyspeck.slackmacgap",
                AppCategory::Personal,
            ),
            rule(AppRuleKind::Website, "docs.google.com", AppCategory::Work),
        ];
        assert_eq!(
            resolve(&ctx("com.tinyspeck.slackmacgap", None), &overrides),
            (AppCategory::Personal, CategorySource::Override)
        );
        assert_eq!(
            resolve(
                &ctx("com.google.Chrome", Some("docs.google.com")),
                &overrides
            ),
            (AppCategory::Work, CategorySource::Override)
        );
        // Subdomains match, lookalikes don't.
        assert_eq!(
            resolve(
                &ctx("com.apple.Safari", Some("x.docs.google.com")),
                &overrides
            )
            .0,
            AppCategory::Work
        );
        assert_eq!(
            resolve(
                &ctx("com.apple.Safari", Some("notdocs.google.com")),
                &overrides
            )
            .0,
            AppCategory::Other
        );
    }

    #[test]
    fn recent_contexts_dedupe() {
        let mut settings = crate::settings::get_default_settings();
        assert!(remember_recent(
            &mut settings,
            &ctx("com.example.editor", None)
        ));
        assert!(!remember_recent(
            &mut settings,
            &ctx("com.example.editor", None)
        ));
        assert!(remember_recent(
            &mut settings,
            &ctx("com.google.Chrome", Some("docs.google.com"))
        ));
        assert_eq!(settings.recent_contexts[0].key, "docs.google.com");
        assert_eq!(settings.recent_contexts.len(), 2);
    }
}
