//! Address bar ("omnibox") input interpretation and suggestion ranking.

use url::Url;

use crate::search::SearchEngine;

/// What the user meant by typing something in the address bar.
#[derive(Debug, Clone, PartialEq)]
pub enum Interpretation {
    Navigate(Url),
    Search {
        engine_id: String,
        terms: String,
        url: Url,
    },
}

impl Interpretation {
    pub fn url(&self) -> &Url {
        match self {
            Self::Navigate(u) => u,
            Self::Search { url, .. } => url,
        }
    }
}

const KNOWN_SCHEMES: &[&str] = &[
    "http",
    "https",
    "file",
    "data",
    "about",
    "ferro",
    "view-source",
    "blob",
    "ftp",
    "ws",
    "wss",
];

/// Common TLDs used to decide whether `foo.bar` looks like a host.
fn looks_like_tld(tld: &str) -> bool {
    tld.len() >= 2 && tld.chars().all(|c| c.is_alphabetic())
}

/// Interpret raw omnibox input as either a URL or a search.
pub fn interpret(
    input: &str,
    default_engine: &SearchEngine,
    engines: &[SearchEngine],
) -> Option<Interpretation> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Keyword search: "w rust language"
    if let Some((kw, rest)) = input.split_once(' ')
        && let Some(engine) = engines
            .iter()
            .find(|e| !e.keyword.is_empty() && e.keyword == kw)
    {
        let terms = rest.trim();
        if !terms.is_empty() {
            return Some(Interpretation::Search {
                engine_id: engine.id.clone(),
                terms: terms.to_string(),
                url: engine.search_url(terms)?,
            });
        }
    }

    // "?query" forces a search.
    if let Some(q) = input.strip_prefix('?') {
        let q = q.trim();
        return Some(search(default_engine, q));
    }

    if !input.contains(char::is_whitespace)
        && let Some(url) = as_url(input)
    {
        return Some(Interpretation::Navigate(url));
    }

    Some(search(default_engine, input))
}

fn search(engine: &SearchEngine, terms: &str) -> Interpretation {
    Interpretation::Search {
        engine_id: engine.id.clone(),
        terms: terms.to_string(),
        url: engine
            .search_url(terms)
            .unwrap_or_else(|| Url::parse("about:blank").unwrap()),
    }
}

/// Try to interpret a single token as a URL.
pub fn as_url(input: &str) -> Option<Url> {
    // Explicit scheme.
    if let Some((scheme, _)) = input.split_once(':')
        && KNOWN_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
    {
        return Url::parse(input).ok();
    }
    // Absolute file paths.
    if input.starts_with('/') || input.starts_with("~/") {
        let path = if let Some(rest) = input.strip_prefix("~/") {
            std::env::home_dir()?.join(rest)
        } else {
            std::path::PathBuf::from(input)
        };
        return Url::from_file_path(path).ok();
    }
    #[cfg(windows)]
    if input.len() > 2
        && input.as_bytes()[1] == b':'
        && (input.as_bytes()[2] == b'\\' || input.as_bytes()[2] == b'/')
    {
        return Url::from_file_path(input).ok();
    }

    // host[:port][/path]
    let host_port = input.split(['/', '?', '#']).next().unwrap_or("");
    let host = match host_port.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => h,
        _ => host_port,
    };
    let is_localhost = host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost");
    let is_ip =
        host.parse::<std::net::IpAddr>().is_ok() || (host.starts_with('[') && host.ends_with(']'));
    let is_domain = host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '.')
        && host.rsplit('.').next().is_some_and(looks_like_tld);
    if is_localhost || is_ip || is_domain {
        let scheme = if is_localhost || is_ip {
            "http"
        } else {
            "https"
        };
        return Url::parse(&format!("{scheme}://{input}")).ok();
    }
    None
}

/// Upgrade `http://` to `https://` unless the host is local.
pub fn https_upgrade(url: &Url) -> Option<Url> {
    if url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?;
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return None;
    }
    let mut u = url.clone();
    u.set_scheme("https").ok()?;
    if u.port() == Some(80) {
        let _ = u.set_port(None);
    }
    Some(u)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SuggestionKind {
    /// "Go to <url>" / "Search for <terms>" for exactly what was typed.
    WhatYouTyped,
    OpenTab,
    Bookmark,
    History,
    SearchSuggestion,
    Command,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub title: String,
    pub url: String,
    pub score: u32,
}

/// Merge and de-duplicate suggestions by URL keeping the best score; sort by score.
pub fn rank(mut suggestions: Vec<Suggestion>, limit: usize) -> Vec<Suggestion> {
    suggestions.sort_by(|a, b| b.score.cmp(&a.score).then(a.kind.cmp(&b.kind)));
    let mut seen = std::collections::HashSet::new();
    suggestions.retain(|s| seen.insert(normalize_for_dedup(&s.url)));
    suggestions.truncate(limit);
    suggestions
}

fn normalize_for_dedup(url: &str) -> String {
    crate::util::pretty_url(url).to_lowercase()
}

/// Inline autocompletion: if `input` is a prefix of a known host, return the completion.
pub fn inline_completion<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    if input.is_empty() || input.contains(' ') {
        return None;
    }
    let lower = input.to_lowercase();
    for candidate in candidates {
        let pretty = crate::util::pretty_url(candidate);
        let host = pretty.split('/').next().unwrap_or("");
        if host.len() > lower.len() && host.to_lowercase().starts_with(&lower) {
            return Some(host.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::builtin_engines;

    fn go(s: &str) -> Interpretation {
        let engines = builtin_engines();
        interpret(s, &engines[0], &engines).unwrap()
    }

    #[test]
    fn urls() {
        assert_eq!(go("example.com").url().as_str(), "https://example.com/");
        assert_eq!(
            go("localhost:3000/x").url().as_str(),
            "http://localhost:3000/x"
        );
        assert_eq!(go("127.0.0.1").url().as_str(), "http://127.0.0.1/");
        assert_eq!(go("about:blank").url().as_str(), "about:blank");
        assert_eq!(go("ferro://settings").url().as_str(), "ferro://settings");
        assert_eq!(go("http://x.org").url().as_str(), "http://x.org/");
        assert!(matches!(go("/tmp/a.html"), Interpretation::Navigate(u) if u.scheme() == "file"));
    }

    #[test]
    fn searches() {
        assert!(matches!(go("rust servo"), Interpretation::Search { .. }));
        assert!(matches!(go("hello"), Interpretation::Search { .. }));
        assert!(matches!(go("?example.com"), Interpretation::Search { .. }));
        match go("w rust language") {
            Interpretation::Search {
                engine_id, terms, ..
            } => {
                assert_eq!(engine_id, "wikipedia");
                assert_eq!(terms, "rust language");
            }
            other => panic!("{other:?}"),
        }
        // file.rs is not a domain-ish TLD? "rs" is a real TLD, so it navigates.
        assert!(matches!(go("v1.2"), Interpretation::Search { .. }));
    }

    #[test]
    fn upgrade() {
        let u = Url::parse("http://example.com:80/a").unwrap();
        assert_eq!(https_upgrade(&u).unwrap().as_str(), "https://example.com/a");
        assert!(https_upgrade(&Url::parse("http://localhost/").unwrap()).is_none());
    }

    #[test]
    fn inline() {
        let c = ["https://github.com/servo", "https://gitlab.com"];
        assert_eq!(inline_completion("gi", c).unwrap(), "github.com");
        assert!(inline_completion("github.com", c).is_none());
    }

    #[test]
    fn ranking() {
        let s = |u: &str, score| Suggestion {
            kind: SuggestionKind::History,
            title: String::new(),
            url: u.into(),
            score,
        };
        let r = rank(
            vec![
                s("https://a.com/", 1),
                s("http://a.com", 5),
                s("https://b.com", 3),
            ],
            10,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].score, 5);
    }
}
