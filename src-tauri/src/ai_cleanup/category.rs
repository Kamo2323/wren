//! Which kind of app the text is going into, from its process / app name and (for browsers) its
//! window title. Pure and table-tested: Windows `.exe` names, macOS display names, browser titles.
//!
//! Privacy: the title is only read here, on this computer. It is never logged or sent to Claude;
//! only the resulting category shapes the prompt.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppCategory {
    /// Coding agents, AI chats, editors and terminals: text becomes a prompt for an AI tool.
    Coding,
    /// Email clients and webmail: email-ready paragraphs.
    Email,
    /// Chat apps: short and casual.
    Messaging,
    /// Everything else: punctuation, capitals and obvious mis-hearings only.
    General,
}

impl AppCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            AppCategory::Coding => "coding",
            AppCategory::Email => "email",
            AppCategory::Messaging => "messaging",
            AppCategory::General => "general",
        }
    }
}

/// Coding agents, editors and IDEs (normalised names: lower case, no `.exe` / `.app`).
const CODING_APPS: &[&str] = &[
    "claude", "claude code", "codex", "t3 code", "t3code", "code", "visual studio code", "code - insiders", "cursor",
    "windsurf", "zed", "antigravity", "sublime_text", "sublime text", "xcode", "devenv", "visual studio", "fleet",
    "nvim-qt", "neovide", "studio64", "android studio", "intellij idea", "idea64", "pycharm", "pycharm64", "webstorm",
    "webstorm64", "rider", "rider64", "clion", "clion64", "goland", "goland64", "phpstorm", "phpstorm64", "rubymine",
    "rubymine64", "datagrip", "datagrip64", "kiro", "trae", "void", "positron",
];

/// Terminals and shells count as coding apps (Claude Code, Codex CLI…).
const TERMINALS: &[&str] = &[
    "windowsterminal", "wt", "windows terminal", "powershell", "pwsh", "cmd", "conhost", "openconsole", "wezterm",
    "wezterm-gui", "alacritty", "kitty", "hyper", "tabby", "warp", "terminal", "iterm", "iterm2", "ghostty", "mintty",
    "git-bash", "conemu", "conemu64", "cmder", "rio", "termius",
];

const EMAIL_APPS: &[&str] = &[
    "outlook", "olk", "microsoft outlook", "mail", "hxoutlook", "thunderbird", "spark", "spark desktop", "airmail",
    "superhuman", "mimestream", "canary mail", "postbox", "mailclient", "em client", "mailspring", "proton mail",
    "betterbird",
];

const MESSAGING_APPS: &[&str] = &[
    "slack", "ms-teams", "teams", "microsoft teams", "microsoft teams (work or school)", "whatsapp", "messages",
    "discord", "telegram", "telegram desktop", "signal", "messenger", "skype", "element", "beeper", "wechat", "line",
    "viber", "google chat", "zulip", "mattermost", "rocket.chat",
];

const BROWSERS: &[&str] = &[
    "chrome", "google chrome", "msedge", "microsoft edge", "firefox", "brave", "brave browser", "arc", "safari", "opera",
    "vivaldi", "chromium", "zen", "zen browser", "orion", "dia", "comet", "librewolf", "waterfox", "floorp",
    "applicationframehost",
];

/// Site names as they appear as one segment of a browser tab title ("Inbox (3) - me@x.com - Gmail").
const EMAIL_SITES: &[&str] = &["gmail", "outlook", "outlook.com", "proton mail", "yahoo mail", "fastmail", "icloud mail", "hey"];
const MESSAGING_SITES: &[&str] = &[
    "whatsapp", "whatsapp web", "slack", "discord", "messenger", "microsoft teams", "teams", "telegram", "telegram web",
    "google chat", "signal", "linkedin messaging",
];
const CODING_SITES: &[&str] = &[
    "claude", "claude.ai", "chatgpt", "codex", "gemini", "github copilot", "copilot", "t3 chat", "vscode.dev",
    "github.dev", "lovable", "bolt", "v0", "replit", "codespaces",
];

/// Lower case, without `.exe` / `.app` and a trailing "(beta)"-style tag.
fn normalise_app(name: &str) -> String {
    let mut n = name.trim().to_lowercase();
    for ext in [".exe", ".app"] {
        if let Some(s) = n.strip_suffix(ext) {
            n = s.to_string();
        }
    }
    if let Some(open) = n.rfind(" (")
        && n.ends_with(')')
        && ["(beta)", "(preview)", "(canary)", "(nightly)", "(insiders)", "(dev)"].contains(&&n[open + 1..])
    {
        n.truncate(open);
    }
    n.trim().to_string()
}

/// Names with an edition or version after them ("PyCharm Professional", "IntelliJ IDEA CE").
const CODING_PREFIXES: &[&str] = &["t3 code", "intellij idea", "pycharm", "webstorm", "android studio", "visual studio code"];

fn in_list(list: &[&str], app: &str) -> bool {
    list.contains(&app)
}

/// The tab title's segments ("Inbox (3) - me@x.com - Gmail" → ["inbox", "me@x.com", "gmail"]),
/// lower case, with unread counters like "(3)" removed and the browser's own name dropped.
fn title_segments(title: &str) -> Vec<String> {
    let mut t = title.to_lowercase();
    for sep in [" – ", " — ", " | ", " · ", " • "] {
        t = t.replace(sep, " - ");
    }
    t.split(" - ")
        .map(|s| {
            let s = s.trim();
            // "(3) WhatsApp", "Inbox (3)": drop counters.
            let s = s.trim_start_matches(|c: char| c == '(' || c.is_ascii_digit() || c == ')' || c == '+' || c == ',' || c == '.' || c == ' ');
            let s = match s.rfind(" (") {
                Some(i) if s.ends_with(')') && s[i + 2..s.len() - 1].chars().all(|c| c.is_ascii_digit() || c == '+' || c == ',' || c == '.') => &s[..i],
                _ => s,
            };
            // Edge writes "Microsoft\u{200b} Edge".
            s.replace('\u{200b}', "").trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn title_category(title: &str) -> Option<AppCategory> {
    let segs = title_segments(title);
    let has = |sites: &[&str]| segs.iter().any(|s| sites.contains(&s.as_str()));
    if has(EMAIL_SITES) {
        Some(AppCategory::Email)
    } else if has(MESSAGING_SITES) {
        Some(AppCategory::Messaging)
    } else if has(CODING_SITES) {
        Some(AppCategory::Coding)
    } else {
        None
    }
}

/// The category for an app. Browsers are judged by their tab title; other apps by name.
pub fn category(process_name: &str, window_title: &str) -> AppCategory {
    let app = normalise_app(process_name);
    if in_list(CODING_APPS, &app) || in_list(TERMINALS, &app) || CODING_PREFIXES.iter().any(|p| app.starts_with(p)) {
        return AppCategory::Coding;
    }
    if in_list(EMAIL_APPS, &app) {
        return AppCategory::Email;
    }
    if in_list(MESSAGING_APPS, &app) {
        return AppCategory::Messaging;
    }
    if in_list(BROWSERS, &app) {
        return title_category(window_title).unwrap_or(AppCategory::General);
    }
    AppCategory::General
}

#[cfg(test)]
mod tests {
    use super::*;
    use AppCategory::*;

    #[test]
    fn category_table() {
        for (app, title, want) in [
            // Windows process names.
            ("claude.exe", "Claude", Coding),
            ("Codex.exe", "", Coding),
            ("T3 Code (Beta).exe", "T3 Code", Coding),
            ("code.exe", "main.rs - wren - Visual Studio Code", Coding),
            ("Cursor.exe", "", Coding),
            ("Antigravity.exe", "", Coding),
            ("idea64.exe", "", Coding),
            ("WindowsTerminal.exe", "Administrator: PowerShell", Coding),
            ("powershell.exe", "", Coding),
            ("pwsh.exe", "", Coding),
            ("cmd.exe", "", Coding),
            ("OUTLOOK.EXE", "Inbox - me@example.com - Outlook", Email),
            ("olk.exe", "", Email),
            ("thunderbird.exe", "", Email),
            ("slack.exe", "", Messaging),
            ("ms-teams.exe", "Chat | Sam | Microsoft Teams", Messaging),
            ("WhatsApp.exe", "", Messaging),
            ("Discord.exe", "#general | Wren", Messaging),
            ("Telegram.exe", "", Messaging),
            ("WINWORD.EXE", "Report.docx - Word", General),
            ("notepad.exe", "Untitled - Notepad", General),
            ("Notion.exe", "Claude notes", General),
            ("", "", General),
            // macOS display names.
            ("Claude", "", Coding),
            ("Codex", "", Coding),
            ("T3 Code", "", Coding),
            ("Visual Studio Code", "", Coding),
            ("Terminal", "zsh", Coding),
            ("iTerm2", "", Coding),
            ("Warp", "", Coding),
            ("Ghostty", "", Coding),
            ("Xcode", "", Coding),
            ("PyCharm Professional Edition", "", Coding),
            ("Mail", "Inbox", Email),
            ("Microsoft Outlook", "", Email),
            ("Spark Desktop", "", Email),
            ("Messages", "", Messaging),
            ("Slack", "", Messaging),
            ("Microsoft Teams", "", Messaging),
            ("WhatsApp", "", Messaging),
            ("Pages", "", General),
            ("Notes", "", General),
            // Browsers: judged by the tab title.
            ("chrome.exe", "Inbox (3) - me@gmail.com - Gmail - Google Chrome", Email),
            ("msedge.exe", "Mail - Sam Jones - Outlook - Personal - Microsoft\u{200b} Edge", Email),
            ("firefox.exe", "Inbox — Proton Mail — Mozilla Firefox", Email),
            ("chrome.exe", "(2) WhatsApp - Google Chrome", Messaging),
            ("chrome.exe", "(1,234) WhatsApp - Google Chrome", Messaging),
            ("Safari", "(1.234) Slack - Acme", Messaging),
            ("brave.exe", "general (Wren) - Slack - Brave", Messaging),
            ("msedge.exe", "Chat | Sam | Microsoft Teams - Work - Microsoft Edge", Messaging),
            ("Google Chrome", "#help | Wren Server - Discord", Messaging),
            ("Safari", "Messenger", Messaging),
            ("chrome.exe", "Fixing the paste bug - Claude - Google Chrome", Coding),
            ("Arc", "ChatGPT", Coding),
            ("chrome.exe", "Wren pull request #6 - GitHub - Google Chrome", General),
            ("chrome.exe", "Agile teams handbook - Google Docs - Google Chrome", General),
            ("chrome.exe", "How to use Gmail filters - YouTube - Google Chrome", General),
            ("chrome.exe", "", General),
            // A title is only read for browsers.
            ("notepad.exe", "Gmail", General),
        ] {
            assert_eq!(category(app, title), want, "{app:?} / {title:?}");
        }
    }

    #[test]
    fn names_are_normalised() {
        assert_eq!(normalise_app("T3 Code (Beta).exe"), "t3 code");
        assert_eq!(normalise_app("Code - Insiders.exe"), "code - insiders");
        assert_eq!(normalise_app("Slack.app"), "slack");
        assert_eq!(normalise_app("Foo (2).exe"), "foo (2)", "only release tags are dropped");
    }
}
