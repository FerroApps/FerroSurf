//! Misc string/URL utilities shared by the UI.

use url::Url;

/// Human readable host for display, without `www.`.
pub fn display_host(url: &Url) -> String {
    match url.host_str() {
        Some(h) => h.trim_start_matches("www.").to_string(),
        None => url.scheme().to_string(),
    }
}

/// Shorten a URL for display in tab tooltips / history lists.
pub fn pretty_url(url: &str) -> String {
    let s = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    s.trim_end_matches('/').to_string()
}

/// Escape a string for inclusion in HTML text or attribute values.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Reverse of [`html_escape`] for the entities commonly found in bookmark exports.
pub fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            },
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            },
            None => {
                out.push('&');
                rest = &rest[1..];
            },
        }
    }
    out.push_str(rest);
    out
}

/// Fuzzy-ish match score: all query words must appear (case-insensitive) in haystack.
/// Returns a score where prefix and word-boundary matches rank higher.
pub fn match_score(query: &str, haystack: &str) -> Option<u32> {
    let hay = haystack.to_lowercase();
    let mut score = 0u32;
    for word in query.split_whitespace() {
        let word = word.to_lowercase();
        let pos = hay.find(&word)?;
        score += 10;
        if pos == 0 {
            score += 20;
        } else if !hay.as_bytes()[pos - 1].is_ascii_alphanumeric() {
            score += 8;
        }
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let s = "a<b> & \"c\" 'd' ñ";
        assert_eq!(html_unescape(&html_escape(s)), s);
        assert_eq!(html_unescape("&#65;&#x42;&bogus; & x"), "AB&bogus; & x");
    }

    #[test]
    fn scoring() {
        assert!(match_score("rust", "The Rust Book").is_some());
        assert!(match_score("rust go", "The Rust Book").is_none());
        assert!(match_score("rust", "rust-lang.org").unwrap() > match_score("rust", "trust me").unwrap());
    }

    #[test]
    fn pretty() {
        assert_eq!(pretty_url("https://www.example.com/"), "example.com");
    }
}
