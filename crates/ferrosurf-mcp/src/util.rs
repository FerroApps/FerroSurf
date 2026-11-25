//! Small shared helpers.

/// Build a reqwest client; loopback targets never go through a proxy.
pub(crate) fn http_client_for(url: &str) -> reqwest::Client {
    let loopback = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| matches!(h, "localhost" | "127.0.0.1" | "::1" | "[::1]")))
        .unwrap_or(false);
    let mut b = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15));
    if loopback {
        b = b.no_proxy();
    }
    b.build().unwrap_or_default()
}

/// Truncate to at most `max` chars (on a char boundary), appending an ellipsis marker.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

pub(crate) fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}
