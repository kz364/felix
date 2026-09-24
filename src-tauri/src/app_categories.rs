//! Automatic app/website → category assignment for per-app style. User
//! overrides (settings `app_rules`) always win; otherwise:
//!
//! 1. a curated list of well-known apps and websites,
//! 2. the app's declared App Store category (`LSApplicationCategoryType`):
//!    social networking → Personal, business → Work,
//! 3. a "mail" hint in the app's bundle id or name → Email,
//! 4. Other.

use crate::settings::AppCategory;

const KNOWN_APPS: &[(&str, AppCategory)] = &[
    // Personal messaging
    ("com.apple.MobileSMS", AppCategory::Personal),
    ("net.whatsapp.WhatsApp", AppCategory::Personal),
    ("ru.keepcoder.Telegram", AppCategory::Personal),
    ("com.tdesktop.Telegram", AppCategory::Personal),
    ("org.whispersystems.signal-desktop", AppCategory::Personal),
    ("com.hnc.Discord", AppCategory::Personal),
    ("com.facebook.archon", AppCategory::Personal),
    ("com.apple.FaceTime", AppCategory::Personal),
    // Work messaging and tools
    ("com.tinyspeck.slackmacgap", AppCategory::Work),
    ("com.microsoft.teams2", AppCategory::Work),
    ("com.microsoft.teams", AppCategory::Work),
    ("us.zoom.xos", AppCategory::Work),
    ("com.linear", AppCategory::Work),
    ("notion.id", AppCategory::Work),
    ("com.atlassian.trello", AppCategory::Work),
    ("com.figma.Desktop", AppCategory::Work),
    ("com.loom.desktop", AppCategory::Work),
    ("com.asana.nativeapp", AppCategory::Work),
    ("com.clickup.desktop-app", AppCategory::Work),
    // Email
    ("com.apple.mail", AppCategory::Email),
    ("com.microsoft.Outlook", AppCategory::Email),
    ("com.superhuman.electron", AppCategory::Email),
    ("com.readdle.SparkDesktop", AppCategory::Email),
    ("com.readdle.smartemail-Mac", AppCategory::Email),
    ("com.mimestream.Mimestream", AppCategory::Email),
    ("it.bloop.airmail2", AppCategory::Email),
    ("com.freron.MailMate", AppCategory::Email),
    ("ch.protonmail.desktop", AppCategory::Email),
];

/// Built-in website assignments (domain, display name, category).
pub const KNOWN_WEBSITES: &[(&str, &str, AppCategory)] = &[
    ("mail.google.com", "Gmail", AppCategory::Email),
    ("outlook.live.com", "Outlook", AppCategory::Email),
    ("outlook.office.com", "Outlook", AppCategory::Email),
    ("mail.proton.me", "Proton Mail", AppCategory::Email),
    ("app.fastmail.com", "Fastmail", AppCategory::Email),
    ("mail.superhuman.com", "Superhuman", AppCategory::Email),
    ("app.slack.com", "Slack", AppCategory::Work),
    ("teams.microsoft.com", "Microsoft Teams", AppCategory::Work),
    ("linear.app", "Linear", AppCategory::Work),
    ("notion.so", "Notion", AppCategory::Work),
    ("atlassian.net", "Jira / Confluence", AppCategory::Work),
    ("web.whatsapp.com", "WhatsApp", AppCategory::Personal),
    ("messenger.com", "Messenger", AppCategory::Personal),
    ("instagram.com", "Instagram", AppCategory::Personal),
    ("discord.com", "Discord", AppCategory::Personal),
    ("web.telegram.org", "Telegram", AppCategory::Personal),
];

/// Automatic category for an app.
pub fn auto_app_category(
    bundle_id: &str,
    name: &str,
    declared_category: Option<&str>,
) -> AppCategory {
    if let Some((_, category)) = KNOWN_APPS.iter().find(|(id, _)| *id == bundle_id) {
        return *category;
    }
    match declared_category {
        Some("public.app-category.social-networking") => return AppCategory::Personal,
        Some("public.app-category.business") => return AppCategory::Work,
        _ => {}
    }
    let hint = format!("{} {}", bundle_id.to_lowercase(), name.to_lowercase());
    if hint.contains("mail") {
        return AppCategory::Email;
    }
    AppCategory::Other
}

pub fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches("www.").to_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Automatic category for a website, if it is a known one.
pub fn auto_website_category(host: &str) -> Option<AppCategory> {
    KNOWN_WEBSITES
        .iter()
        .filter(|(domain, _, _)| host_matches(host, domain))
        .max_by_key(|(domain, _, _)| domain.len())
        .map(|(_, _, category)| *category)
}

/// `LSApplicationCategoryType` of an app bundle.
#[cfg(target_os = "macos")]
pub fn declared_category_at(path: &str) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(path))?;
    let value =
        bundle.objectForInfoDictionaryKey(&NSString::from_str("LSApplicationCategoryType"))?;
    value.downcast::<NSString>().ok().map(|s| s.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn declared_category_at(_path: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_win() {
        assert_eq!(
            auto_app_category(
                "com.apple.mail",
                "Mail",
                Some("public.app-category.productivity")
            ),
            AppCategory::Email
        );
        assert_eq!(
            auto_app_category(
                "com.linear",
                "Linear",
                Some("public.app-category.developer-tools")
            ),
            AppCategory::Work
        );
    }

    #[test]
    fn declared_category_and_hints() {
        assert_eq!(
            auto_app_category(
                "com.example.chat",
                "Chatty",
                Some("public.app-category.social-networking")
            ),
            AppCategory::Personal
        );
        assert_eq!(
            auto_app_category(
                "com.example.crm",
                "CRM",
                Some("public.app-category.business")
            ),
            AppCategory::Work
        );
        assert_eq!(
            auto_app_category("com.example.hey", "HEY Mail", None),
            AppCategory::Email
        );
        assert_eq!(
            auto_app_category(
                "com.apple.Notes",
                "Notes",
                Some("public.app-category.productivity")
            ),
            AppCategory::Other
        );
    }

    #[test]
    fn websites() {
        assert_eq!(
            auto_website_category("mail.google.com"),
            Some(AppCategory::Email)
        );
        assert_eq!(
            auto_website_category("acme.atlassian.net"),
            Some(AppCategory::Work)
        );
        assert_eq!(auto_website_category("docs.google.com"), None);
    }
}
