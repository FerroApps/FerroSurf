//! Search engines and keyword search.

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchEngine {
    pub id: String,
    pub name: String,
    /// Search URL with `{searchTerms}` placeholder.
    pub url: String,
    /// Optional OpenSearch-JSON suggestions URL with `{searchTerms}` placeholder.
    #[serde(default)]
    pub suggest_url: Option<String>,
    /// Omnibox keyword, e.g. `w` → "w rust" searches Wikipedia.
    #[serde(default)]
    pub keyword: String,
}

impl SearchEngine {
    fn new(id: &str, name: &str, url: &str, suggest: Option<&str>, keyword: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            url: url.into(),
            suggest_url: suggest.map(Into::into),
            keyword: keyword.into(),
        }
    }

    pub fn search_url(&self, terms: &str) -> Option<Url> {
        Url::parse(&self.url.replace("{searchTerms}", &encode(terms))).ok()
    }

    pub fn suggest_url(&self, terms: &str) -> Option<Url> {
        let template = self.suggest_url.as_ref()?;
        Url::parse(&template.replace("{searchTerms}", &encode(terms))).ok()
    }

    pub fn host(&self) -> Option<String> {
        Url::parse(&self.url.replace("{searchTerms}", "x"))
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
    }
}

/// application/x-www-form-urlencoded style encoding of search terms.
pub fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

pub fn builtin_engines() -> Vec<SearchEngine> {
    vec![
        SearchEngine::new(
            "duckduckgo",
            "DuckDuckGo",
            "https://duckduckgo.com/?q={searchTerms}",
            Some("https://duckduckgo.com/ac/?q={searchTerms}&type=list"),
            "d",
        ),
        SearchEngine::new(
            "google",
            "Google",
            "https://www.google.com/search?q={searchTerms}",
            Some("https://suggestqueries.google.com/complete/search?client=firefox&q={searchTerms}"),
            "g",
        ),
        SearchEngine::new(
            "bing",
            "Bing",
            "https://www.bing.com/search?q={searchTerms}",
            Some("https://api.bing.com/osjson.aspx?query={searchTerms}"),
            "b",
        ),
        SearchEngine::new(
            "brave",
            "Brave Search",
            "https://search.brave.com/search?q={searchTerms}",
            Some("https://search.brave.com/api/suggest?q={searchTerms}"),
            "br",
        ),
        SearchEngine::new(
            "startpage",
            "Startpage",
            "https://www.startpage.com/do/search?q={searchTerms}",
            None,
            "sp",
        ),
        SearchEngine::new(
            "ecosia",
            "Ecosia",
            "https://www.ecosia.org/search?q={searchTerms}",
            None,
            "eco",
        ),
        SearchEngine::new(
            "kagi",
            "Kagi",
            "https://kagi.com/search?q={searchTerms}",
            None,
            "k",
        ),
        SearchEngine::new(
            "wikipedia",
            "Wikipedia",
            "https://en.wikipedia.org/wiki/Special:Search?search={searchTerms}",
            Some("https://en.wikipedia.org/w/api.php?action=opensearch&search={searchTerms}"),
            "w",
        ),
        SearchEngine::new(
            "github",
            "GitHub",
            "https://github.com/search?q={searchTerms}",
            None,
            "gh",
        ),
        SearchEngine::new(
            "crates",
            "crates.io",
            "https://crates.io/search?q={searchTerms}",
            None,
            "crate",
        ),
        SearchEngine::new(
            "youtube",
            "YouTube",
            "https://www.youtube.com/results?search_query={searchTerms}",
            None,
            "yt",
        ),
    ]
}

/// Parse an OpenSearch suggestions JSON response: `["query", ["s1", "s2", ...]]`.
pub fn parse_suggestions(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    // OpenSearch: ["q", [..]]
    if let Some(arr) = value.get(1).and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .take(8)
            .collect();
    }
    // DuckDuckGo legacy: [{"phrase": ".."}]
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|v| v.get("phrase").and_then(|p| p.as_str()).map(str::to_string))
            .take(8)
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let e = &builtin_engines()[0];
        assert_eq!(
            e.search_url("rust & servo").unwrap().as_str(),
            "https://duckduckgo.com/?q=rust+%26+servo"
        );
        assert_eq!(e.host().unwrap(), "duckduckgo.com");
    }

    #[test]
    fn suggestions() {
        assert_eq!(parse_suggestions(r#"["ru",["rust","ruby"]]"#), vec!["rust", "ruby"]);
        assert_eq!(parse_suggestions(r#"[{"phrase":"rust"}]"#), vec!["rust"]);
        assert!(parse_suggestions("nope").is_empty());
    }
}
