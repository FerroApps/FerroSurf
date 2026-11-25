//! Example plugin: the `request` hook. Redirects Twitter/X and YouTube page
//! loads to configurable privacy-friendly frontends. Everything is off
//! until the user turns it on in the settings.

use ferrosurf_plugin_sdk::prelude::*;

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Config {
    pub twitter: bool,
    pub twitter_host: String,
    pub youtube: bool,
    pub youtube_host: String,
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.contains('.')
        && h.len() <= 100
        && h.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
}

/// `(host, path and query)` of an `http(s)` URL.
fn split(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(end);
    Some((host, if tail.is_empty() { "/" } else { tail }))
}

fn is_host(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|p| p.ends_with('.'))
}

/// The URL to go to instead, if any.
pub fn redirect(url: &str, cfg: &Config) -> Option<String> {
    let (host, tail) = split(url)?;
    let host = host.to_ascii_lowercase();
    if cfg.twitter
        && valid_host(&cfg.twitter_host)
        && host != cfg.twitter_host
        && (is_host(&host, "twitter.com") || is_host(&host, "x.com"))
    {
        return Some(format!("https://{}{tail}", cfg.twitter_host));
    }
    if cfg.youtube && valid_host(&cfg.youtube_host) && host != cfg.youtube_host {
        if host == "youtu.be" {
            let id = tail.trim_start_matches('/');
            let id = id.split(['?', '#', '/']).next().unwrap_or("");
            if !id.is_empty() {
                return Some(format!("https://{}/watch?v={id}", cfg.youtube_host));
            }
        } else if is_host(&host, "youtube.com") {
            let p = tail.split(['?', '#']).next().unwrap_or("/");
            let page = ["/watch", "/shorts/", "/playlist", "/channel/", "/@", "/c/", "/results"];
            if page.iter().any(|s| p.starts_with(s)) {
                return Some(format!("https://{}{tail}", cfg.youtube_host));
            }
        }
    }
    None
}

#[derive(Default)]
pub struct PrivacyRedirects {
    cfg: Config,
}

impl PrivacyRedirects {
    fn reload(&mut self, get: impl Fn(&str) -> Option<SettingValue>) {
        let b = |k: &str| get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let s = |k: &str| {
            get(k)
                .and_then(|v| v.as_str().map(|s| s.trim().to_ascii_lowercase()))
                .unwrap_or_default()
        };
        self.cfg = Config {
            twitter: b("twitter_enabled"),
            twitter_host: s("twitter_host"),
            youtube: b("youtube_enabled"),
            youtube_host: s("youtube_host"),
        };
    }
}

impl Plugin for PrivacyRedirects {
    fn load(&mut self, info: LoadInfo) {
        self.reload(|k| info.settings.get(k).cloned());
    }

    fn setting_changed(&mut self, _key: String, _value: SettingValue) {
        self.reload(host::setting);
    }

    fn request(&mut self, req: RequestInput) -> RequestOutput {
        // Only page loads: embeds and API calls keep working.
        if !req.main_frame {
            return RequestOutput::allow();
        }
        match redirect(&req.url, &self.cfg) {
            Some(to) => RequestOutput::redirect(to),
            None => RequestOutput::allow(),
        }
    }
}

register_plugin!(PrivacyRedirects);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            twitter: true,
            twitter_host: "nitter.net".into(),
            youtube: true,
            youtube_host: "piped.video".into(),
        }
    }

    #[test]
    fn twitter_and_x() {
        let c = cfg();
        assert_eq!(
            redirect("https://twitter.com/rustlang/status/1?s=2", &c).as_deref(),
            Some("https://nitter.net/rustlang/status/1?s=2")
        );
        assert_eq!(
            redirect("https://mobile.x.com/a", &c).as_deref(),
            Some("https://nitter.net/a")
        );
        assert_eq!(redirect("https://nitter.net/a", &c), None);
        assert_eq!(redirect("https://notx.com/a", &c), None);
    }

    #[test]
    fn youtube() {
        let c = cfg();
        assert_eq!(
            redirect("https://www.youtube.com/watch?v=abc", &c).as_deref(),
            Some("https://piped.video/watch?v=abc")
        );
        assert_eq!(
            redirect("https://youtu.be/abc?t=4", &c).as_deref(),
            Some("https://piped.video/watch?v=abc")
        );
        assert_eq!(redirect("https://www.youtube.com/", &c), None);
    }

    #[test]
    fn off_by_default_and_bad_hosts() {
        assert_eq!(redirect("https://twitter.com/a", &Config::default()), None);
        let mut c = cfg();
        c.twitter_host = "evil host/".into();
        assert_eq!(redirect("https://twitter.com/a", &c), None);
    }

    #[test]
    fn settings_drive_the_hook() {
        let mut p = PrivacyRedirects::default();
        p.reload(|k| match k {
            "twitter_enabled" => Some(SettingValue::Bool(true)),
            "twitter_host" => Some(SettingValue::Str(" Nitter.Example ".into())),
            _ => None,
        });
        let r = p.request(RequestInput {
            url: "https://x.com/a".into(),
            main_frame: true,
            ..Default::default()
        });
        assert_eq!(r.redirect_url.as_deref(), Some("https://nitter.example/a"));
        let sub = p.request(RequestInput {
            url: "https://x.com/a".into(),
            main_frame: false,
            ..Default::default()
        });
        assert_eq!(sub.action, RequestAction::Allow);
    }
}
