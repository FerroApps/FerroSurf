//! WebExtension match patterns (`<all_urls>`, `*://*.example.com/*`, ...).

use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PatternError {
    #[error("empty match pattern")]
    Empty,
    #[error("match pattern is missing '://'")]
    MissingSchemeSeparator,
    #[error("invalid scheme in match pattern: {0:?}")]
    InvalidScheme(String),
    #[error("match pattern is missing a path")]
    MissingPath,
    #[error("invalid host in match pattern: {0:?}")]
    InvalidHost(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Scheme {
    /// `*`: http, https, ws, wss
    Wild,
    Exact(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    Any,
    Exact(String),
    /// `*.suffix`: the suffix itself and any subdomain.
    Sub(String),
    /// `file:///` style (empty host).
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    AllUrls,
    Pattern {
        scheme: Scheme,
        host: Host,
        path: String,
    },
}

/// A parsed WebExtension match pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchPattern {
    raw: String,
    kind: Kind,
}

const ALL_URLS_SCHEMES: &[&str] = &["http", "https", "ws", "wss", "ftp", "file"];
const WILD_SCHEMES: &[&str] = &["http", "https", "ws", "wss"];

impl MatchPattern {
    pub fn parse(s: &str) -> Result<Self, PatternError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(PatternError::Empty);
        }
        if s == "<all_urls>" {
            return Ok(Self {
                raw: s.to_owned(),
                kind: Kind::AllUrls,
            });
        }
        let (scheme_s, rest) = s
            .split_once("://")
            .ok_or(PatternError::MissingSchemeSeparator)?;
        let scheme = if scheme_s == "*" {
            Scheme::Wild
        } else {
            let valid = scheme_s
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme_s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
            if !valid {
                return Err(PatternError::InvalidScheme(scheme_s.to_owned()));
            }
            Scheme::Exact(scheme_s.to_ascii_lowercase())
        };
        let slash = rest.find('/').ok_or(PatternError::MissingPath)?;
        let (host_s, path) = rest.split_at(slash);
        let host = if host_s.is_empty() {
            if scheme == Scheme::Exact("file".into()) {
                Host::Empty
            } else {
                return Err(PatternError::InvalidHost(host_s.to_owned()));
            }
        } else if host_s == "*" {
            Host::Any
        } else if let Some(suffix) = host_s.strip_prefix("*.") {
            Self::check_host(host_s, suffix)?;
            Host::Sub(suffix.to_ascii_lowercase())
        } else {
            Self::check_host(host_s, host_s)?;
            Host::Exact(host_s.to_ascii_lowercase())
        };
        Ok(Self {
            raw: s.to_owned(),
            kind: Kind::Pattern {
                scheme,
                host,
                path: path.to_owned(),
            },
        })
    }

    fn check_host(full: &str, h: &str) -> Result<(), PatternError> {
        let bad = h.is_empty()
            || h.contains(['*', ':', '@', '?', '#', ' '])
            || h.starts_with('.')
            || h.ends_with('.')
            || h.contains("..");
        if bad {
            Err(PatternError::InvalidHost(full.to_owned()))
        } else {
            Ok(())
        }
    }

    /// The original pattern text.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, url: &Url) -> bool {
        let scheme = url.scheme();
        match &self.kind {
            Kind::AllUrls => ALL_URLS_SCHEMES.contains(&scheme),
            Kind::Pattern {
                scheme: ps,
                host,
                path,
            } => {
                let scheme_ok = match ps {
                    Scheme::Wild => WILD_SCHEMES.contains(&scheme),
                    Scheme::Exact(e) => e == scheme,
                };
                if !scheme_ok {
                    return false;
                }
                let h = url.host_str().unwrap_or("");
                let host_ok = match host {
                    Host::Any => true,
                    Host::Empty => h.is_empty(),
                    Host::Exact(e) => h.eq_ignore_ascii_case(e),
                    Host::Sub(suf) => {
                        let h = h.to_ascii_lowercase();
                        h == *suf
                            || (h.len() > suf.len()
                                && h.ends_with(suf.as_str())
                                && h.as_bytes()[h.len() - suf.len() - 1] == b'.')
                    }
                };
                if !host_ok {
                    return false;
                }
                // Path (and query) are matched, the fragment is not.
                if path == "/*" {
                    return true;
                }
                let mut target = String::with_capacity(url.path().len() + 16);
                target.push_str(url.path());
                if let Some(q) = url.query() {
                    target.push('?');
                    target.push_str(q);
                }
                glob_match(path.as_bytes(), target.as_bytes())
            }
        }
    }

    /// A JavaScript regular expression source that matches `location.href`
    /// exactly when [`MatchPattern::matches`] would match the URL.
    pub fn to_js_regex(&self) -> String {
        match &self.kind {
            Kind::AllUrls => r"^(?:https?|wss?|ftp|file)://.*$".to_owned(),
            Kind::Pattern { scheme, host, path } => {
                let mut r = String::from("^");
                match scheme {
                    Scheme::Wild => r.push_str("(?:https?|wss?)"),
                    Scheme::Exact(s) => r.push_str(&re_escape(s)),
                }
                r.push_str("://");
                match host {
                    Host::Any => r.push_str(r"[^/?#]*"),
                    Host::Empty => {}
                    Host::Exact(h) => r.push_str(&re_escape(h)),
                    Host::Sub(s) => {
                        r.push_str(r"(?:[^/?#@:]+\.)?");
                        r.push_str(&re_escape(s));
                    }
                }
                if !matches!(host, Host::Empty) {
                    r.push_str(r"(?::\d+)?");
                }
                for ch in path.chars() {
                    if ch == '*' {
                        r.push_str(".*");
                    } else {
                        push_escaped(&mut r, ch);
                    }
                }
                r.push_str(r"(?:#.*)?$");
                r
            }
        }
    }
}

fn push_escaped(out: &mut String, ch: char) {
    if "\\^$.|?*+()[]{}/".contains(ch) {
        out.push('\\');
    }
    out.push(ch);
}

fn re_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        push_escaped(&mut o, c);
    }
    o
}

/// Glob matching where `*` matches any (possibly empty) sequence.
fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if p < pat.len() && pat[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, u: &str) -> bool {
        MatchPattern::parse(p)
            .unwrap()
            .matches(&Url::parse(u).unwrap())
    }

    #[test]
    fn all_urls() {
        assert!(m("<all_urls>", "https://example.com/"));
        assert!(m("<all_urls>", "http://a.b/x?y"));
        assert!(m("<all_urls>", "file:///tmp/x.html"));
        assert!(!m("<all_urls>", "about:blank"));
        assert!(!m("<all_urls>", "data:text/html,hi"));
    }

    #[test]
    fn wildcard_scheme_and_subdomains() {
        let p = "*://*.example.com/*";
        assert!(m(p, "https://example.com/"));
        assert!(m(p, "http://www.example.com/a/b?c=d"));
        assert!(m(p, "https://a.b.example.com/"));
        assert!(m(p, "wss://x.example.com/sock"));
        assert!(!m(p, "https://notexample.com/"));
        assert!(!m(p, "https://example.com.evil.org/"));
        assert!(!m(p, "ftp://example.com/"));
        assert!(m(p, "https://EXAMPLE.com:8080/"));
    }

    #[test]
    fn exact_host_and_path_glob() {
        assert!(m("https://a.com/path*", "https://a.com/path"));
        assert!(m("https://a.com/path*", "https://a.com/pathology?q=1"));
        assert!(!m("https://a.com/path*", "https://a.com/other"));
        assert!(!m("https://a.com/path*", "http://a.com/path"));
        assert!(!m("https://a.com/path*", "https://www.a.com/path"));
        assert!(m("https://a.com/", "https://a.com"));
        assert!(!m("https://a.com/", "https://a.com/x"));
        assert!(m("https://a.com/*/edit", "https://a.com/x/y/edit"));
        assert!(!m("https://a.com/*/edit", "https://a.com/x/y/edit2"));
        assert!(m("https://a.com/?id=*", "https://a.com/?id=5"));
        assert!(m("https://a.com/x", "https://a.com/x#frag"));
    }

    #[test]
    fn any_host_and_file() {
        assert!(m("https://*/*", "https://foo.bar/x"));
        assert!(!m("https://*/*", "http://foo.bar/x"));
        assert!(m("file:///*", "file:///home/u/a.html"));
        assert!(!m("file:///*", "https://x/"));
        assert!(m("http://localhost/*", "http://localhost:3000/a"));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(MatchPattern::parse("").unwrap_err(), PatternError::Empty);
        assert!(MatchPattern::parse("example.com").is_err());
        assert!(MatchPattern::parse("https://example.com").is_err());
        assert!(MatchPattern::parse("https://*example.com/*").is_err());
        assert!(MatchPattern::parse("https://a.com:80/*").is_err());
        assert!(MatchPattern::parse("https:///*").is_err());
        assert!(MatchPattern::parse("1http://a.com/*").is_err());
        assert!(MatchPattern::parse("https://*./*").is_err());
    }

    #[test]
    fn js_regex_agrees_with_matches() {
        let pats = [
            "<all_urls>",
            "*://*.example.com/*",
            "https://a.com/path*",
            "https://a.com/",
            "file:///*",
            "https://*/*",
            "https://a.com/?id=*",
        ];
        let urls = [
            "https://example.com/",
            "http://www.example.com:81/a?b#c",
            "https://notexample.com/",
            "https://a.com/path",
            "https://a.com/",
            "https://a.com/?id=3",
            "file:///tmp/x",
            "ftp://z.org/x",
            "https://a.com/x#frag",
        ];
        for p in pats {
            let mp = MatchPattern::parse(p).unwrap();
            let re = regex::Regex::new(&mp.to_js_regex()).unwrap();
            for u in urls {
                let url = Url::parse(u).unwrap();
                assert_eq!(mp.matches(&url), re.is_match(url.as_str()), "{p} vs {u}");
            }
        }
    }
}
