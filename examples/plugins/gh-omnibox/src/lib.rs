//! Example plugin: omnibox keyword `gh`. Needs no permissions.

use ferrosurf_plugin_sdk::prelude::*;

/// Percent-encodes everything but unreserved characters.
pub fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `owner`, `owner/repo` or `owner/repo/path`: safe to put in a URL path.
fn looks_like_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('/')
        && !s.contains("//")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

pub fn suggest(text: &str) -> OmniboxOutput {
    let text = text.trim();
    if text.is_empty() {
        return OmniboxOutput {
            suggestions: vec![Suggestion {
                title: "GitHub".into(),
                url: "https://github.com/".into(),
            }],
            navigate: None,
        };
    }
    let mut suggestions = Vec::new();
    if looks_like_path(text) {
        suggestions.push(Suggestion {
            title: format!("github.com/{text}"),
            url: format!("https://github.com/{text}"),
        });
    }
    suggestions.push(Suggestion {
        title: format!("Buscar «{text}» en GitHub"),
        url: format!(
            "https://github.com/search?q={}&type=repositories",
            encode_query(text)
        ),
    });
    let navigate = suggestions.first().map(|s| s.url.clone());
    OmniboxOutput {
        suggestions,
        navigate,
    }
}

#[derive(Default)]
pub struct GhOmnibox;

impl Plugin for GhOmnibox {
    fn omnibox(&mut self, input: OmniboxInput) -> OmniboxOutput {
        suggest(&input.text)
    }
}

register_plugin!(GhOmnibox);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_paths_and_search() {
        let o = suggest("rust-lang/rust");
        assert_eq!(o.suggestions[0].url, "https://github.com/rust-lang/rust");
        assert_eq!(o.navigate.as_deref(), Some("https://github.com/rust-lang/rust"));
        assert!(o.suggestions[1].url.ends_with("q=rust-lang%2Frust&type=repositories"));
    }

    #[test]
    fn free_text_only_searches() {
        let o = suggest("wasm runtime & more");
        assert_eq!(o.suggestions.len(), 1);
        assert!(o.suggestions[0].url.contains("q=wasm+runtime+%26+more"));
        assert_eq!(suggest("").suggestions[0].url, "https://github.com/");
        assert_eq!(suggest("../etc").suggestions.len(), 2);
    }
}
