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

/// Record the frontmost app now and start reading the browser URL.
pub fn capture() {
    let (app_name, bundle_id) = frontmost_app();
    let ctx = DictationContext {
        app_name,
        bundle_id: bundle_id.clone(),
        url_host: None,
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

#[cfg(target_os = "macos")]
fn frontmost_app() -> (Option<String>, Option<String>) {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    match workspace.frontmostApplication() {
        Some(app) => (
            app.localizedName().map(|s| s.to_string()),
            app.bundleIdentifier().map(|s| s.to_string()),
        ),
        None => (None, None),
    }
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app() -> (Option<String>, Option<String>) {
    (None, None)
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

fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches("www.").to_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Category for a context: website rules win inside a browser, then app
/// rules, else Other. Returns the matching rule, if any.
pub fn resolve<'a>(
    ctx: &DictationContext,
    rules: &'a [AppRule],
) -> (AppCategory, Option<&'a AppRule>) {
    if let Some(host) = &ctx.url_host {
        // Most specific domain first ("mail.google.com" over "google.com").
        if let Some(rule) = rules
            .iter()
            .filter(|r| r.kind == AppRuleKind::Website && host_matches(host, &r.key))
            .max_by_key(|r| r.key.len())
        {
            return (rule.category, Some(rule));
        }
    }
    if let Some(bundle_id) = &ctx.bundle_id {
        if let Some(rule) = rules
            .iter()
            .find(|r| r.kind == AppRuleKind::App && r.key == *bundle_id)
        {
            return (rule.category, Some(rule));
        }
    }
    (AppCategory::Other, None)
}

/// The rule the Style page should offer for an unassigned context: the
/// website when in a browser tab, otherwise the app.
pub fn unassigned_entry(ctx: &DictationContext) -> Option<AppRule> {
    if let Some(host) = &ctx.url_host {
        return Some(AppRule {
            kind: AppRuleKind::Website,
            key: host.clone(),
            label: host.clone(),
            category: AppCategory::Other,
        });
    }
    let bundle_id = ctx.bundle_id.clone()?;
    if bundle_id == "com.pais.handy" {
        return None;
    }
    Some(AppRule {
        kind: AppRuleKind::App,
        label: ctx.app_name.clone().unwrap_or_else(|| bundle_id.clone()),
        key: bundle_id,
        category: AppCategory::Other,
    })
}

/// Remember an unassigned context (newest first, max 12). Returns true when
/// the list changed and settings need saving.
pub fn remember_recent(settings: &mut AppSettings, ctx: &DictationContext) -> bool {
    let Some(entry) = unassigned_entry(ctx) else {
        return false;
    };
    if resolve(ctx, &settings.app_rules).1.is_some() {
        return false;
    }
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
    use crate::settings::default_app_rules;

    fn ctx(bundle: &str, host: Option<&str>) -> DictationContext {
        DictationContext {
            app_name: Some("App".into()),
            bundle_id: Some(bundle.into()),
            url_host: host.map(str::to_string),
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
    fn resolves_websites_before_apps() {
        let rules = default_app_rules();
        assert_eq!(
            resolve(&ctx("com.google.Chrome", Some("mail.google.com")), &rules).0,
            AppCategory::Email
        );
        assert_eq!(
            resolve(&ctx("com.google.Chrome", Some("docs.google.com")), &rules).0,
            AppCategory::Other
        );
        assert_eq!(
            resolve(&ctx("com.tinyspeck.slackmacgap", None), &rules).0,
            AppCategory::Work
        );
        assert_eq!(
            resolve(&ctx("com.apple.MobileSMS", None), &rules).0,
            AppCategory::Personal
        );
        assert_eq!(
            resolve(&ctx("com.unknown.app", None), &rules).0,
            AppCategory::Other
        );
    }

    #[test]
    fn subdomains_match_but_lookalikes_do_not() {
        let rules = vec![AppRule {
            kind: AppRuleKind::Website,
            key: "notion.so".into(),
            label: "Notion".into(),
            category: AppCategory::Work,
        }];
        assert_eq!(
            resolve(&ctx("com.apple.Safari", Some("team.notion.so")), &rules).0,
            AppCategory::Work
        );
        assert_eq!(
            resolve(&ctx("com.apple.Safari", Some("notnotion.so")), &rules).0,
            AppCategory::Other
        );
    }

    #[test]
    fn recent_contexts_dedupe_and_skip_assigned() {
        let mut settings = crate::settings::get_default_settings();
        assert!(remember_recent(
            &mut settings,
            &ctx("com.example.editor", None)
        ));
        assert!(!remember_recent(
            &mut settings,
            &ctx("com.example.editor", None)
        ));
        assert!(!remember_recent(
            &mut settings,
            &ctx("com.tinyspeck.slackmacgap", None)
        ));
        assert!(remember_recent(
            &mut settings,
            &ctx("com.google.Chrome", Some("docs.google.com"))
        ));
        assert_eq!(settings.recent_contexts[0].key, "docs.google.com");
        assert_eq!(settings.recent_contexts.len(), 2);
    }
}
