use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::FocusSnapshot;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureFilter {
    pub include_apps: Vec<String>,
    pub exclude_apps: Vec<String>,
    pub exclude_titles: Vec<String>,
    pub include_urls: Vec<String>,
    pub exclude_urls: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    OwnApp,
    AppExcluded,
    AppNotIncluded,
    TitleExcluded,
    UrlExcluded,
    UrlNotIncluded,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnApp => "own_app",
            Self::AppExcluded => "app_excluded",
            Self::AppNotIncluded => "app_not_included",
            Self::TitleExcluded => "title_excluded",
            Self::UrlExcluded => "url_excluded",
            Self::UrlNotIncluded => "url_not_included",
        }
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CaptureFilter {
    pub fn with_default_excludes() -> Self {
        Self {
            exclude_apps: ["1Password", "Bitwarden", "Keychain Access", "KeePass", "KeePassXC"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ..Self::default()
        }
    }

    pub fn check_window(&self, focus: &FocusSnapshot) -> Result<(), SkipReason> {
        let app_matches = |name: &String| {
            name.eq_ignore_ascii_case(&focus.app_name)
                || name.eq_ignore_ascii_case(&focus.identity.app_id)
                || executable_name(&focus.identity.app_id)
                    .is_some_and(|exe| name.eq_ignore_ascii_case(exe))
        };
        if self.exclude_apps.iter().any(app_matches) {
            return Err(SkipReason::AppExcluded);
        }
        if !self.include_apps.is_empty() && !self.include_apps.iter().any(app_matches) {
            return Err(SkipReason::AppNotIncluded);
        }
        let title = focus.window_title.to_lowercase();
        if self
            .exclude_titles
            .iter()
            .any(|pattern| !pattern.is_empty() && title.contains(&pattern.to_lowercase()))
        {
            return Err(SkipReason::TitleExcluded);
        }
        Ok(())
    }

    pub fn check_url(&self, url: Option<&str>) -> Result<(), SkipReason> {
        let Some(url) = url else {
            return Ok(());
        };
        if self.exclude_urls.iter().any(|pattern| url_matches(pattern, url)) {
            return Err(SkipReason::UrlExcluded);
        }
        if !self.include_urls.is_empty()
            && !self.include_urls.iter().any(|pattern| url_matches(pattern, url))
        {
            return Err(SkipReason::UrlNotIncluded);
        }
        Ok(())
    }
}

fn executable_name(app_id: &str) -> Option<&str> {
    let file = app_id.rsplit(['/', '\\']).next()?;
    Some(file.strip_suffix(".exe").or(file.strip_suffix(".EXE")).unwrap_or(file))
}

fn url_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    if pattern.is_empty() {
        return false;
    }
    let url = url.to_lowercase();
    if pattern.contains("://") {
        return url.starts_with(&pattern);
    }
    let Some(host) = host_of(&url) else {
        return false;
    };
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FocusIdentity, WindowRect};

    fn focus(app: &str, app_id: &str, title: &str) -> FocusSnapshot {
        FocusSnapshot {
            identity: FocusIdentity {
                pid: 7,
                window_id: 1,
                app_id: app_id.into(),
            },
            app_name: app.into(),
            window_title: title.into(),
            rect: WindowRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
                scale: 1,
            },
            display_id: 1,
        }
    }

    #[test]
    fn excluded_app_matches_name_bundle_id_or_exe() {
        let filter = CaptureFilter {
            exclude_apps: vec!["com.apple.keychainaccess".into(), "slack".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(
            filter.check_window(&focus("Keychain Access", "com.apple.keychainaccess", "x")),
            Err(SkipReason::AppExcluded)
        );
        assert_eq!(
            filter.check_window(&focus("Slack", r"C:\Apps\slack.exe", "x")),
            Err(SkipReason::AppExcluded)
        );
        assert_eq!(filter.check_window(&focus("TextEdit", "com.apple.TextEdit", "x")), Ok(()));
    }

    #[test]
    fn include_list_drops_every_other_app() {
        let filter = CaptureFilter {
            include_apps: vec!["TextEdit".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(filter.check_window(&focus("TextEdit", "com.apple.TextEdit", "a")), Ok(()));
        assert_eq!(
            filter.check_window(&focus("Notes", "com.apple.Notes", "a")),
            Err(SkipReason::AppNotIncluded)
        );
    }

    #[test]
    fn exclusion_wins_over_inclusion() {
        let filter = CaptureFilter {
            include_apps: vec!["Safari".into()],
            exclude_apps: vec!["Safari".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(
            filter.check_window(&focus("Safari", "com.apple.Safari", "a")),
            Err(SkipReason::AppExcluded)
        );
    }

    #[test]
    fn title_patterns_are_case_insensitive_substrings() {
        let filter = CaptureFilter {
            exclude_titles: vec!["private browsing".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(
            filter.check_window(&focus("Safari", "com.apple.Safari", "News - Private Browsing")),
            Err(SkipReason::TitleExcluded)
        );
    }

    #[test]
    fn url_host_patterns_match_subdomains_but_not_lookalikes() {
        let filter = CaptureFilter {
            exclude_urls: vec!["bank.com".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(
            filter.check_url(Some("https://login.bank.com/account")),
            Err(SkipReason::UrlExcluded)
        );
        assert_eq!(
            filter.check_url(Some("https://user:pw@bank.com:8443/")),
            Err(SkipReason::UrlExcluded)
        );
        assert_eq!(filter.check_url(Some("https://notbank.com/")), Ok(()));
        assert_eq!(filter.check_url(Some("https://example.com/?r=bank.com")), Ok(()));
    }

    #[test]
    fn url_prefix_patterns_and_include_list() {
        let filter = CaptureFilter {
            include_urls: vec!["https://github.com/dhruvdabhi101/".into()],
            ..CaptureFilter::default()
        };
        assert_eq!(filter.check_url(Some("https://github.com/dhruvdabhi101/goku")), Ok(()));
        assert_eq!(
            filter.check_url(Some("https://github.com/other/repo")),
            Err(SkipReason::UrlNotIncluded)
        );
        assert_eq!(filter.check_url(None), Ok(()));
    }
}
