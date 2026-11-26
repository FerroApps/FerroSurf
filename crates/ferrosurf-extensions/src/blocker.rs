//! Network content blocker: Adblock Plus / uBlock network filters plus Chrome
//! `declarativeNetRequest` rules, behind one fast matching engine.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use url::Url;

// ---------------------------------------------------------------------------
// Public request / decision types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    MainFrame,
    SubFrame,
    Script,
    Stylesheet,
    Image,
    Font,
    Xhr,
    Media,
    WebSocket,
    Ping,
    Other,
}

impl ResourceType {
    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

const T_MAIN: u16 = ResourceType::MainFrame.bit();
const T_SUB: u16 = ResourceType::SubFrame.bit();
const T_SCRIPT: u16 = ResourceType::Script.bit();
const T_STYLE: u16 = ResourceType::Stylesheet.bit();
const T_IMAGE: u16 = ResourceType::Image.bit();
const T_FONT: u16 = ResourceType::Font.bit();
const T_XHR: u16 = ResourceType::Xhr.bit();
const T_MEDIA: u16 = ResourceType::Media.bit();
const T_WS: u16 = ResourceType::WebSocket.bit();
const T_PING: u16 = ResourceType::Ping.bit();
const T_OTHER: u16 = ResourceType::Other.bit();
const ALL_TYPES: u16 = (1 << 11) - 1;
const DEFAULT_TYPES: u16 = ALL_TYPES & !T_MAIN;

#[derive(Debug, Clone, Copy)]
pub struct RequestInfo<'a> {
    pub url: &'a Url,
    /// Top-level page / referrer that caused the request.
    pub initiator: Option<&'a Url>,
    pub resource_type: ResourceType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
    Redirect(Url),
    UpgradeScheme,
}

// ---------------------------------------------------------------------------
// DNR (Chrome declarativeNetRequest) JSON model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DnrActionType {
    Block,
    Allow,
    Redirect,
    UpgradeScheme,
    AllowAllRequests,
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrTransform {
    pub scheme: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrRedirect {
    pub url: Option<String>,
    /// Resolved to `url` by the extension manager (needs the extension id).
    pub extension_path: Option<String>,
    pub transform: Option<DnrTransform>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnrAction {
    #[serde(rename = "type")]
    pub kind: DnrActionType,
    #[serde(default)]
    pub redirect: Option<DnrRedirect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnrResourceType {
    MainFrame,
    SubFrame,
    Stylesheet,
    Script,
    Image,
    Font,
    Object,
    Xmlhttprequest,
    Ping,
    CspReport,
    Media,
    Websocket,
    Webtransport,
    Webbundle,
    Other,
    #[serde(other)]
    Unknown,
}

impl DnrResourceType {
    fn bit(self) -> u16 {
        match self {
            Self::MainFrame => T_MAIN,
            Self::SubFrame => T_SUB,
            Self::Stylesheet => T_STYLE,
            Self::Script => T_SCRIPT,
            Self::Image => T_IMAGE,
            Self::Font => T_FONT,
            Self::Xmlhttprequest => T_XHR,
            Self::Ping | Self::CspReport => T_PING,
            Self::Media => T_MEDIA,
            Self::Websocket => T_WS,
            Self::Object | Self::Webtransport | Self::Webbundle | Self::Other | Self::Unknown => {
                T_OTHER
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DnrDomainType {
    FirstParty,
    ThirdParty,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrCondition {
    pub url_filter: Option<String>,
    pub regex_filter: Option<String>,
    pub request_domains: Option<Vec<String>>,
    pub excluded_request_domains: Option<Vec<String>>,
    #[serde(alias = "domains")]
    pub initiator_domains: Option<Vec<String>>,
    #[serde(alias = "excludedDomains")]
    pub excluded_initiator_domains: Option<Vec<String>>,
    pub resource_types: Option<Vec<DnrResourceType>>,
    pub excluded_resource_types: Option<Vec<DnrResourceType>>,
    pub domain_type: Option<DnrDomainType>,
}

fn default_priority() -> i32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrRule {
    pub id: u32,
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub action: DnrAction,
    #[serde(default)]
    pub condition: DnrCondition,
}

// ---------------------------------------------------------------------------
// Internal representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(String),
    Star,
    Caret,
}

#[derive(Debug)]
enum Pat {
    /// Matches every URL.
    Any,
    /// `||host<rest>`: indexed by host suffix. `rest` is matched right after
    /// the host name in the URL.
    Host {
        host: String,
        rest: Vec<Tok>,
        end: bool,
    },
    Tokens {
        toks: Vec<Tok>,
        start: bool,
        end: bool,
        /// Match must begin at a label boundary inside the host (`||` fallback).
        dom: bool,
    },
    Regex(regex::Regex),
}

#[derive(Debug)]
enum RedirectTarget {
    Url(Url),
    Transform(DnrTransform),
}

impl RedirectTarget {
    fn apply(&self, from: &Url) -> Option<Url> {
        match self {
            Self::Url(u) => Some(u.clone()),
            Self::Transform(t) => {
                let mut u = from.clone();
                if let Some(s) = &t.scheme {
                    u.set_scheme(s).ok()?;
                }
                if let Some(h) = &t.host {
                    u.set_host(Some(h)).ok()?;
                }
                if let Some(p) = &t.port {
                    u.set_port(if p.is_empty() {
                        None
                    } else {
                        Some(p.parse().ok()?)
                    })
                    .ok()?;
                }
                if let Some(p) = &t.path {
                    u.set_path(p);
                }
                Some(u)
            }
        }
    }
}

#[derive(Debug)]
enum Act {
    Block,
    Allow,
    Redirect(RedirectTarget),
    Upgrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Third {
    Any,
    Only,
    Not,
}

#[derive(Debug)]
struct Rule {
    pat: Pat,
    act: Act,
    prio: i32,
    rank: u8,
    types: u16,
    third: Third,
    init_inc: Vec<Box<str>>,
    init_exc: Vec<Box<str>>,
    req_inc: Vec<Box<str>>,
    req_exc: Vec<Box<str>>,
}

struct Ctx<'a> {
    lc: &'a str,
    host: Option<&'a str>,
    host_start: usize,
    host_end: usize,
    init_host: Option<&'a str>,
    third: bool,
    rt: u16,
}

/// DNR rules are offset above ABP-style rules so that explicit extension
/// rules win over list rules of the same kind.
const DNR_PRIO_BASE: i32 = 10;

// ---------------------------------------------------------------------------
// Pattern parsing / matching
// ---------------------------------------------------------------------------

struct ParsedPat {
    toks: Vec<Tok>,
    start: bool,
    end: bool,
    domain: bool,
}

fn parse_pattern(p: &str) -> ParsedPat {
    let lower = p.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let (mut domain, mut start, mut end) = (false, false, false);
    if let Some(r) = rest.strip_prefix("||") {
        domain = true;
        rest = r;
    } else if let Some(r) = rest.strip_prefix('|') {
        start = true;
        rest = r;
    }
    if let Some(r) = rest.strip_suffix('|') {
        end = true;
        rest = r;
    }
    let mut toks: Vec<Tok> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, toks: &mut Vec<Tok>| {
        if !buf.is_empty() {
            toks.push(Tok::Lit(std::mem::take(buf)));
        }
    };
    for c in rest.chars() {
        match c {
            '*' => {
                flush(&mut buf, &mut toks);
                if toks.last() != Some(&Tok::Star) {
                    toks.push(Tok::Star);
                }
            }
            '^' => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Caret);
            }
            c => buf.push(c),
        }
    }
    flush(&mut buf, &mut toks);
    if !domain && !start && toks.first() == Some(&Tok::Star) {
        toks.remove(0);
    }
    if start && toks.first() == Some(&Tok::Star) {
        toks.remove(0);
        start = false;
    }
    if toks.last() == Some(&Tok::Star) {
        toks.pop();
        end = false;
    }
    ParsedPat {
        toks,
        start,
        end,
        domain,
    }
}

fn host_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'-'
}

fn build_pat(pp: ParsedPat) -> Pat {
    let ParsedPat {
        toks,
        start,
        end,
        domain,
    } = pp;
    if toks.is_empty() && !start && !domain {
        return Pat::Any;
    }
    if domain {
        if let Some(Tok::Lit(first)) = toks.first() {
            let b = first.as_bytes();
            let n = b.iter().take_while(|c| host_char(**c)).count();
            let host = &first[..n];
            let well_formed = n > 0
                && !host.starts_with('.')
                && !host.ends_with('.')
                && !host.contains("..")
                && host.contains(|c: char| c.is_ascii_alphabetic() || c == '.');
            let boundary_ok = if n < first.len() {
                matches!(b[n], b'/' | b':' | b'?')
            } else {
                matches!(toks.get(1), None | Some(Tok::Caret))
            };
            if well_formed && boundary_ok {
                let mut rest = Vec::new();
                if n < first.len() {
                    rest.push(Tok::Lit(first[n..].to_owned()));
                }
                rest.extend(toks[1..].iter().cloned());
                return Pat::Host {
                    host: host.to_owned(),
                    rest,
                    end,
                };
            }
        }
        return Pat::Tokens {
            toks,
            start: false,
            end,
            dom: true,
        };
    }
    Pat::Tokens {
        toks,
        start,
        end,
        dom: false,
    }
}

fn is_sep(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

fn match_toks(toks: &[Tok], s: &[u8], pos: usize, end: bool) -> bool {
    match toks.split_first() {
        None => !end || pos == s.len(),
        Some((Tok::Lit(l), rest)) => {
            s[pos..].starts_with(l.as_bytes()) && match_toks(rest, s, pos + l.len(), end)
        }
        Some((Tok::Caret, rest)) => {
            if pos == s.len() {
                match_toks(rest, s, pos, end)
            } else if is_sep(s[pos]) {
                match_toks(rest, s, pos + 1, end)
            } else {
                false
            }
        }
        Some((Tok::Star, rest)) => {
            rest.is_empty() || (pos..=s.len()).any(|p| match_toks(rest, s, p, end))
        }
    }
}

fn match_unanchored(toks: &[Tok], text: &str, end: bool, dom: Option<(usize, usize)>) -> bool {
    let b = text.as_bytes();
    let dom_ok = |p: usize| match dom {
        None => true,
        Some((hs, he)) => p >= hs && p <= he && (p == hs || b[p - 1] == b'.'),
    };
    if let Some(Tok::Lit(l)) = toks.first() {
        let mut from = 0;
        while from <= b.len() {
            let Some(i) = text[from..].find(l.as_str()) else {
                return false;
            };
            let p = from + i;
            if dom_ok(p) && match_toks(toks, b, p, end) {
                return true;
            }
            from = p + 1;
        }
        false
    } else {
        (0..=b.len()).any(|p| dom_ok(p) && match_toks(toks, b, p, end))
    }
}

/// Longest alphanumeric run (>= 3 chars) that is a whole token of any matching URL.
fn keyword(toks: &[Tok], start: bool, end: bool) -> Option<String> {
    let mut best: Option<&str> = None;
    for (i, t) in toks.iter().enumerate() {
        let Tok::Lit(l) = t else { continue };
        let b = l.as_bytes();
        let mut j = 0;
        while j < b.len() {
            if !b[j].is_ascii_alphanumeric() {
                j += 1;
                continue;
            }
            let a = j;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let left = a > 0 || (i == 0 && start) || (i > 0 && toks[i - 1] == Tok::Caret);
            let right = j < b.len()
                || (i + 1 == toks.len() && end)
                || (i + 1 < toks.len() && toks[i + 1] == Tok::Caret);
            if left && right && j - a >= 3 && best.is_none_or(|x| x.len() < j - a) {
                best = Some(&l[a..j]);
            }
        }
    }
    best.map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Host helpers
// ---------------------------------------------------------------------------

const MULTI_PART_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "ltd.uk",
    "plc.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.jp",
    "ne.jp",
    "or.jp",
    "ac.jp",
    "go.jp",
    "co.nz",
    "org.nz",
    "net.nz",
    "co.in",
    "net.in",
    "org.in",
    "com.br",
    "net.br",
    "org.br",
    "com.cn",
    "net.cn",
    "org.cn",
    "com.mx",
    "co.kr",
    "or.kr",
    "co.za",
    "com.tr",
    "com.tw",
    "com.hk",
    "com.sg",
    "com.ar",
    "com.co",
    "co.id",
    "co.il",
    "com.ua",
    "com.pl",
    "github.io",
    "gitlab.io",
    "herokuapp.com",
    "blogspot.com",
    "appspot.com",
    "netlify.app",
    "vercel.app",
    "pages.dev",
    "workers.dev",
    "cloudfront.net",
    "azurewebsites.net",
    "web.app",
    "firebaseapp.com",
    "s3.amazonaws.com",
    "myshopify.com",
    "wordpress.com",
];

/// Approximate eTLD+1.
fn reg_domain(host: &str) -> &str {
    if host.starts_with('[') || host.as_bytes().last().is_some_and(|c| c.is_ascii_digit()) {
        return host; // IP address (or numeric host)
    }
    let mut dots = host.rmatch_indices('.').map(|(i, _)| i);
    if dots.next().is_none() {
        return host;
    }
    let Some(d2) = dots.next() else { return host };
    let last_two = &host[d2 + 1..];
    if MULTI_PART_SUFFIXES.contains(&last_two) {
        match dots.next() {
            Some(d3) => &host[d3 + 1..],
            None => host,
        }
    } else {
        last_two
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

fn any_domain(host: &str, list: &[Box<str>]) -> bool {
    list.iter().any(|d| domain_matches(host, d))
}

fn token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

// ---------------------------------------------------------------------------
// Rule evaluation
// ---------------------------------------------------------------------------

impl Rule {
    fn matches(&self, c: &Ctx) -> bool {
        if self.types & c.rt == 0 {
            return false;
        }
        match self.third {
            Third::Only if !c.third => return false,
            Third::Not if c.third => return false,
            _ => {}
        }
        let b = c.lc.as_bytes();
        let ok = match &self.pat {
            Pat::Any => true,
            Pat::Host { rest, end, .. } => {
                c.host.is_some() && match_toks(rest, b, c.host_end, *end)
            }
            Pat::Tokens {
                toks,
                start,
                end,
                dom,
            } => {
                if *start {
                    match_toks(toks, b, 0, *end)
                } else {
                    let d = if *dom {
                        if c.host.is_none() {
                            return false;
                        }
                        Some((c.host_start, c.host_end))
                    } else {
                        None
                    };
                    match_unanchored(toks, c.lc, *end, d)
                }
            }
            Pat::Regex(re) => re.is_match(c.lc),
        };
        if !ok {
            return false;
        }
        if !self.init_inc.is_empty() && !c.init_host.is_some_and(|h| any_domain(h, &self.init_inc))
        {
            return false;
        }
        if !self.init_exc.is_empty() && c.init_host.is_some_and(|h| any_domain(h, &self.init_exc)) {
            return false;
        }
        if !self.req_inc.is_empty() && !c.host.is_some_and(|h| any_domain(h, &self.req_inc)) {
            return false;
        }
        if !self.req_exc.is_empty() && c.host.is_some_and(|h| any_domain(h, &self.req_exc)) {
            return false;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Blocker
// ---------------------------------------------------------------------------

const MAX_URL_LEN: usize = 4096;

pub struct Blocker {
    rules: Vec<Rule>,
    by_host: HashMap<Box<str>, Vec<u32>>,
    by_token: HashMap<Box<str>, Vec<u32>>,
    generic: Vec<u32>,
    blocked: AtomicU64,
}

impl Default for Blocker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blocker")
            .field("rules", &self.rules.len())
            .field("blocked", &self.blocked_count())
            .finish()
    }
}

impl Blocker {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            by_host: HashMap::new(),
            by_token: HashMap::new(),
            generic: Vec::new(),
            blocked: AtomicU64::new(0),
        }
    }

    /// A blocker preloaded with the built-in ad/tracker host list.
    pub fn with_builtin_lists() -> Self {
        let mut b = Self::new();
        b.add_filter_list(include_str!("../data/trackers.txt"));
        b
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn blocked_count(&self) -> u64 {
        self.blocked.load(Ordering::Relaxed)
    }

    fn insert(&mut self, rule: Rule) {
        let idx = self.rules.len() as u32;
        match &rule.pat {
            Pat::Host { host, .. } => self
                .by_host
                .entry(host.as_str().into())
                .or_default()
                .push(idx),
            Pat::Any if !rule.req_inc.is_empty() => {
                let mut seen: Vec<&str> = Vec::new();
                for d in &rule.req_inc {
                    if !seen.contains(&&**d) {
                        seen.push(d);
                        self.by_host.entry(d.clone()).or_default().push(idx);
                    }
                }
            }
            Pat::Tokens {
                toks, start, end, ..
            } => match keyword(toks, *start, *end) {
                Some(k) => self.by_token.entry(k.into()).or_default().push(idx),
                None => self.generic.push(idx),
            },
            _ => self.generic.push(idx),
        }
        self.rules.push(rule);
    }

    /// Add Adblock Plus / uBlock network filters. Returns the number of rules added.
    pub fn add_filter_list(&mut self, text: &str) -> usize {
        let mut n = 0;
        for line in text.lines() {
            if let Some(rule) = parse_abp_line(line) {
                self.insert(rule);
                n += 1;
            }
        }
        n
    }

    pub fn add_dnr_rules(&mut self, rules: Vec<DnrRule>) {
        for r in rules {
            if let Some(rule) = convert_dnr(r) {
                self.insert(rule);
            }
        }
    }

    pub fn check(&self, req: &RequestInfo) -> Decision {
        let url = req.url;
        if self.rules.is_empty() || matches!(url.scheme(), "data" | "blob" | "about" | "javascript")
        {
            return Decision::Allow;
        }
        let full = url.as_str();
        let full = if full.len() > MAX_URL_LEN {
            &full[..MAX_URL_LEN]
        } else {
            full
        };
        let lc: Cow<str> = if full.bytes().any(|c| c.is_ascii_uppercase()) {
            Cow::Owned(full.to_ascii_lowercase())
        } else {
            Cow::Borrowed(full)
        };
        let host = url.host_str().filter(|h| !h.is_empty());
        let (mut host_start, mut host_end) = (0, 0);
        let mut host_s = None;
        if let Some(h) = host
            && let Some(i) = lc.find("://")
        {
            let a = i + 3;
            let aend = lc[a..].find(['/', '?', '#']).map_or(lc.len(), |e| a + e);
            let at = lc[a..aend].rfind('@').map_or(0, |x| x + 1);
            let hs = a + at;
            if lc.len() >= hs + h.len()
                && lc.as_bytes()[hs..hs + h.len()].eq_ignore_ascii_case(h.as_bytes())
            {
                host_start = hs;
                host_end = hs + h.len();
                host_s = Some(h);
            }
        }
        let init_host = req
            .initiator
            .and_then(|u| u.host_str())
            .filter(|h| !h.is_empty());
        let third = match (host_s, init_host) {
            (Some(h), Some(i)) => !reg_domain(h).eq_ignore_ascii_case(reg_domain(i)),
            _ => false,
        };
        let ctx = Ctx {
            lc: &lc,
            host: host_s,
            host_start,
            host_end,
            init_host,
            third,
            rt: req.resource_type.bit(),
        };

        let mut best: Option<(i32, u8, u32)> = None;
        let mut consider = |ri: u32| {
            let r = &self.rules[ri as usize];
            if let Some((bp, br, _)) = best
                && (r.prio, r.rank) <= (bp, br)
            {
                return;
            }
            if r.matches(&ctx) {
                best = Some((r.prio, r.rank, ri));
            }
        };

        if let Some(h) = host_s
            && !self.by_host.is_empty()
        {
            let mut start = 0;
            loop {
                if let Some(v) = self.by_host.get(&h[start..]) {
                    v.iter().for_each(|&ri| consider(ri));
                }
                match h[start..].find('.') {
                    Some(i) => start += i + 1,
                    None => break,
                }
            }
        }
        if !self.by_token.is_empty() {
            let b = ctx.lc.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if token_byte(b[i]) {
                    let s = i;
                    while i < b.len() && token_byte(b[i]) {
                        i += 1;
                    }
                    if i - s >= 3
                        && let Some(v) = self.by_token.get(&ctx.lc[s..i])
                    {
                        v.iter().for_each(|&ri| consider(ri));
                    }
                } else {
                    i += 1;
                }
            }
        }
        self.generic.iter().for_each(|&ri| consider(ri));

        let Some((_, _, ri)) = best else {
            return Decision::Allow;
        };
        match &self.rules[ri as usize].act {
            Act::Allow => Decision::Allow,
            Act::Block => {
                self.blocked.fetch_add(1, Ordering::Relaxed);
                Decision::Block
            }
            Act::Upgrade => {
                if matches!(url.scheme(), "http" | "ws") {
                    Decision::UpgradeScheme
                } else {
                    Decision::Allow
                }
            }
            Act::Redirect(t) => t.apply(url).map_or(Decision::Allow, Decision::Redirect),
        }
    }
}

// ---------------------------------------------------------------------------
// ABP filter parsing
// ---------------------------------------------------------------------------

fn abp_type(name: &str) -> Option<u16> {
    Some(match name {
        "script" => T_SCRIPT,
        "image" => T_IMAGE,
        "stylesheet" | "css" => T_STYLE,
        "xmlhttprequest" | "xhr" => T_XHR,
        "subdocument" | "frame" => T_SUB,
        "font" => T_FONT,
        "media" => T_MEDIA,
        "websocket" => T_WS,
        "ping" => T_PING,
        "other" | "object" | "object-subrequest" => T_OTHER,
        "document" | "doc" => T_MAIN,
        _ => return None,
    })
}

fn parse_abp_line(line: &str) -> Option<Rule> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
        return None;
    }
    for cosmetic in ["##", "#@#", "#?#", "#$#", "#%#", "#@$#", "#@?#"] {
        if line.contains(cosmetic) {
            return None;
        }
    }
    let (exception, body) = match line.strip_prefix("@@") {
        Some(b) => (true, b),
        None => (false, line),
    };
    // Split options.
    let (pat_s, opts_s) = match body.rfind('$') {
        Some(i)
            if body[i + 1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "~=|.,-_:".contains(c))
                && !body[i + 1..].is_empty() =>
        {
            (&body[..i], Some(&body[i + 1..]))
        }
        _ => (body, None),
    };

    let mut inc_types = 0u16;
    let mut exc_types = 0u16;
    let mut third = Third::Any;
    let mut important = false;
    let mut init_inc = Vec::new();
    let mut init_exc = Vec::new();
    if let Some(opts) = opts_s {
        for opt in opts.split(',') {
            let opt = opt.trim().to_ascii_lowercase();
            let (neg, name) = match opt.strip_prefix('~') {
                Some(n) => (true, n),
                None => (false, opt.as_str()),
            };
            match name {
                "third-party" | "3p" => third = if neg { Third::Not } else { Third::Only },
                "first-party" | "1p" => third = if neg { Third::Only } else { Third::Not },
                "important" => important = true,
                "match-case" | "collapse" => {}
                "all" => inc_types = ALL_TYPES,
                _ if name.starts_with("domain=") => {
                    for d in name["domain=".len()..].split('|') {
                        if let Some(n) = d.strip_prefix('~') {
                            if !n.is_empty() {
                                init_exc.push(n.to_owned().into_boxed_str());
                            }
                        } else if !d.is_empty() {
                            init_inc.push(d.to_owned().into_boxed_str());
                        }
                    }
                }
                _ => match abp_type(name) {
                    Some(t) if neg => exc_types |= t,
                    Some(t) => inc_types |= t,
                    None => return None, // unsupported option: skip the rule
                },
            }
        }
    }
    let mut types = if inc_types != 0 {
        inc_types
    } else {
        DEFAULT_TYPES
    };
    types &= !exc_types;
    if !exception {
        // Never block top-level navigations from filter lists.
        types &= !T_MAIN;
    }
    if types == 0 {
        return None;
    }

    let pat = if pat_s.len() > 2 && pat_s.starts_with('/') && pat_s.ends_with('/') {
        let re = regex::RegexBuilder::new(&pat_s[1..pat_s.len() - 1])
            .case_insensitive(true)
            .size_limit(1 << 20)
            .build()
            .ok()?;
        Pat::Regex(re)
    } else {
        let pp = parse_pattern(pat_s);
        // A bare "||" / "|" / empty pattern without options is meaningless.
        if pp.toks.is_empty() && opts_s.is_none() {
            return None;
        }
        build_pat(pp)
    };

    let (act, prio, rank) = if exception {
        (Act::Allow, 1, 4)
    } else if important {
        (Act::Block, 2, 2)
    } else {
        (Act::Block, 0, 2)
    };
    Some(Rule {
        pat,
        act,
        prio,
        rank,
        types,
        third,
        init_inc,
        init_exc,
        req_inc: Vec::new(),
        req_exc: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// DNR conversion
// ---------------------------------------------------------------------------

fn lower_list(v: &Option<Vec<String>>) -> Vec<Box<str>> {
    v.iter()
        .flatten()
        .map(|d| d.trim().to_ascii_lowercase().into_boxed_str())
        .filter(|d| !d.is_empty())
        .collect()
}

fn convert_dnr(r: DnrRule) -> Option<Rule> {
    let (act, rank) = match r.action.kind {
        DnrActionType::Block => (Act::Block, 2),
        DnrActionType::Allow => (Act::Allow, 4),
        DnrActionType::AllowAllRequests => (Act::Allow, 3),
        DnrActionType::UpgradeScheme => (Act::Upgrade, 1),
        DnrActionType::Redirect => {
            let rd = r.action.redirect.as_ref()?;
            let target = if let Some(u) = &rd.url {
                RedirectTarget::Url(Url::parse(u).ok()?)
            } else if let Some(t) = &rd.transform {
                RedirectTarget::Transform(t.clone())
            } else {
                return None;
            };
            (Act::Redirect(target), 0)
        }
        DnrActionType::Unsupported => return None,
    };
    let c = &r.condition;
    let pat = if let Some(rf) = &c.regex_filter {
        Pat::Regex(
            regex::RegexBuilder::new(rf)
                .case_insensitive(true)
                .size_limit(1 << 20)
                .build()
                .ok()?,
        )
    } else if let Some(uf) = &c.url_filter {
        build_pat(parse_pattern(uf))
    } else {
        Pat::Any
    };
    let mut types = match &c.resource_types {
        Some(v) => v.iter().fold(0, |m, t| m | t.bit()),
        None => DEFAULT_TYPES,
    };
    if let Some(v) = &c.excluded_resource_types {
        for t in v {
            types &= !t.bit();
        }
    }
    let third = match c.domain_type {
        Some(DnrDomainType::FirstParty) => Third::Not,
        Some(DnrDomainType::ThirdParty) => Third::Only,
        None => Third::Any,
    };
    Some(Rule {
        pat,
        act,
        prio: DNR_PRIO_BASE + r.priority.max(0),
        rank,
        types,
        third,
        init_inc: lower_list(&c.initiator_domains),
        init_exc: lower_list(&c.excluded_initiator_domains),
        req_inc: lower_list(&c.request_domains),
        req_exc: lower_list(&c.excluded_request_domains),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn chk(b: &Blocker, url: &str, init: Option<&str>, t: ResourceType) -> Decision {
        let url = u(url);
        let init = init.map(u);
        b.check(&RequestInfo {
            url: &url,
            initiator: init.as_ref(),
            resource_type: t,
        })
    }

    use ResourceType as R;

    fn bl(list: &str) -> Blocker {
        let mut b = Blocker::new();
        b.add_filter_list(list);
        b
    }

    #[test]
    fn domain_anchor_and_subdomains() {
        let b = bl("||ads.example.com^\n||tracker.net^");
        let p = Some("https://site.org/");
        assert_eq!(
            chk(&b, "https://ads.example.com/x.js", p, R::Script),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://a.b.tracker.net/", p, R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://tracker.net:8080/p", p, R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://example.com/", p, R::Script),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://nottracker.net/", p, R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://tracker.net.evil.org/", p, R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://evil.org/tracker.net/", p, R::Image),
            Decision::Allow
        );
        assert_eq!(b.blocked_count(), 3);
    }

    #[test]
    fn caret_and_path_rules() {
        let b = bl("||example.com/ads/*\n/banner/*/img^\n|https://exact.com/a|");
        assert_eq!(
            chk(&b, "https://example.com/ads/1.png", None, R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://example.com/other", None, R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://x.com/banner/a/b/img?x", None, R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://x.com/banner/a/imgs", None, R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://exact.com/a", None, R::Other),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://exact.com/ab", None, R::Other),
            Decision::Allow
        );
    }

    #[test]
    fn plain_substring_with_wildcard() {
        let b = bl("/adframe.\nads*banner");
        assert_eq!(
            chk(&b, "https://x.com/AdFrame.html", None, R::SubFrame),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://x.com/ads/big_banner.gif", None, R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://x.com/page", None, R::Image),
            Decision::Allow
        );
    }

    #[test]
    fn exceptions_and_important() {
        let b = bl("||ads.com^\n@@||ads.com/ok^\n||evil.com^$important\n@@||evil.com^");
        assert_eq!(
            chk(&b, "https://ads.com/ok/x", None, R::Script),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://ads.com/bad", None, R::Script),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://evil.com/", None, R::Script),
            Decision::Block
        );
    }

    #[test]
    fn third_party_option() {
        let b = bl("||cdn.example.com^$third-party\n||first.org^$~third-party");
        let same = Some("https://www.example.com/");
        let other = Some("https://news.org/");
        assert_eq!(
            chk(&b, "https://cdn.example.com/a.js", same, R::Script),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://cdn.example.com/a.js", other, R::Script),
            Decision::Block
        );
        assert_eq!(
            chk(
                &b,
                "https://first.org/a",
                Some("https://first.org/"),
                R::Script
            ),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://first.org/a", other, R::Script),
            Decision::Allow
        );
        // multi-part suffix: a.co.uk vs b.co.uk are different sites
        assert_eq!(
            chk(
                &b,
                "https://cdn.example.com/a.js",
                Some("https://example.co.uk/"),
                R::Script
            ),
            Decision::Block
        );
        assert_eq!(reg_domain("a.b.co.uk"), "b.co.uk");
        assert_eq!(reg_domain("x.github.io"), "x.github.io");
        assert_eq!(reg_domain("www.example.com"), "example.com");
        assert_eq!(reg_domain("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn resource_type_options() {
        let b = bl("||x.com^$script,image\n||y.com^$~script\n||z.com^$document");
        assert_eq!(chk(&b, "https://x.com/a", None, R::Script), Decision::Block);
        assert_eq!(chk(&b, "https://x.com/a", None, R::Image), Decision::Block);
        assert_eq!(chk(&b, "https://x.com/a", None, R::Xhr), Decision::Allow);
        assert_eq!(chk(&b, "https://y.com/a", None, R::Script), Decision::Allow);
        assert_eq!(chk(&b, "https://y.com/a", None, R::Font), Decision::Block);
        // Filter lists never block main frames.
        assert_eq!(
            chk(&b, "https://y.com/a", None, R::MainFrame),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://z.com/a", None, R::MainFrame),
            Decision::Allow
        );
        let b2 = bl("||x.com^\n");
        assert_eq!(
            chk(&b2, "https://x.com/", None, R::MainFrame),
            Decision::Allow
        );
    }

    #[test]
    fn domain_option() {
        let b = bl("||w.com^$domain=a.com|~sub.a.com");
        assert_eq!(
            chk(&b, "https://w.com/x", Some("https://a.com/"), R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://w.com/x", Some("https://m.a.com/"), R::Image),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://w.com/x", Some("https://sub.a.com/"), R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://w.com/x", Some("https://b.com/"), R::Image),
            Decision::Allow
        );
        assert_eq!(chk(&b, "https://w.com/x", None, R::Image), Decision::Allow);
    }

    #[test]
    fn ignores_cosmetic_comments_and_unsupported() {
        let mut b = Blocker::new();
        let n = b.add_filter_list(
            "[Adblock Plus 2.0]\n! comment\nexample.com##.ad\nexample.com#@#.ad\n\
             ||a.com^$popup\n||b.com^$redirect=noop.js\n||c.com^$csp=x\n||d.com^\n\
             /ad[0-9]+\\.js/\n",
        );
        assert_eq!(n, 2);
        assert_eq!(b.len(), 2);
        assert_eq!(chk(&b, "https://d.com/", None, R::Image), Decision::Block);
        assert_eq!(
            chk(&b, "https://q.com/AD42.js", None, R::Script),
            Decision::Block
        );
        assert_eq!(chk(&b, "https://a.com/", None, R::Image), Decision::Allow);
    }

    #[test]
    fn dnr_chrome_sample() {
        let json = r#"[
          {"id": 1, "priority": 1, "action": {"type": "block"},
           "condition": {"urlFilter": "abc", "domains": ["foo.com"], "resourceTypes": ["script"]}},
          {"id": 2, "priority": 1, "action": {"type": "redirect", "redirect": {"url": "https://example.com/new"}},
           "condition": {"urlFilter": "||old.example.com^", "resourceTypes": ["main_frame"]}},
          {"id": 3, "priority": 2, "action": {"type": "allow"},
           "condition": {"urlFilter": "abc/allowed", "resourceTypes": ["script"]}},
          {"id": 4, "priority": 1, "action": {"type": "upgradeScheme"},
           "condition": {"requestDomains": ["secure.com"], "resourceTypes": ["main_frame", "sub_frame"]}},
          {"id": 5, "priority": 1, "action": {"type": "block"},
           "condition": {"regexFilter": "^https://r\\.com/[0-9]+$", "domainType": "thirdParty"}},
          {"id": 6, "action": {"type": "redirect", "redirect": {"transform": {"scheme": "https"}}},
           "condition": {"urlFilter": "|http://plain.org/", "resourceTypes": ["main_frame"]}},
          {"id": 7, "action": {"type": "modifyHeaders"}, "condition": {}}
        ]"#;
        let v: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        let rules: Vec<DnrRule> = v
            .into_iter()
            .filter_map(|x| serde_json::from_value(x).ok())
            .collect();
        assert_eq!(rules.len(), 7);
        let mut b = Blocker::new();
        b.add_dnr_rules(rules);
        assert_eq!(b.len(), 6); // modifyHeaders skipped
        let foo = Some("https://foo.com/");
        assert_eq!(
            chk(&b, "https://x.com/abc.js", foo, R::Script),
            Decision::Block
        );
        assert_eq!(
            chk(
                &b,
                "https://x.com/abc.js",
                Some("https://bar.com/"),
                R::Script
            ),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://x.com/abc.png", foo, R::Image),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://x.com/abc/allowed.js", foo, R::Script),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://old.example.com/", None, R::MainFrame),
            Decision::Redirect(u("https://example.com/new"))
        );
        assert_eq!(
            chk(&b, "http://secure.com/", None, R::MainFrame),
            Decision::UpgradeScheme
        );
        assert_eq!(
            chk(&b, "https://secure.com/", None, R::MainFrame),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://r.com/123", Some("https://o.com/"), R::Xhr),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://r.com/123", Some("https://r.com/"), R::Xhr),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "http://plain.org/a", None, R::MainFrame),
            Decision::Redirect(u("https://plain.org/a"))
        );
    }

    #[test]
    fn dnr_priority_over_list_rules() {
        let mut b = bl("||ads.com^$important");
        b.add_dnr_rules(vec![
            serde_json::from_str(
                r#"{"id":1,"priority":1,"action":{"type":"allow"},"condition":{"urlFilter":"||ads.com^"}}"#,
            )
            .unwrap(),
        ]);
        assert_eq!(
            chk(&b, "https://ads.com/", None, R::Script),
            Decision::Allow
        );
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn builtin_list() {
        let b = Blocker::with_builtin_lists();
        assert!(b.len() >= 250, "got {}", b.len());
        let p = Some("https://news.example.org/");
        for h in [
            "https://www.googletagmanager.com/gtm.js?id=1",
            "https://stats.g.doubleclick.net/x",
            "https://www.google-analytics.com/analytics.js",
            "https://connect.facebook.net/en_US/fbevents.js",
        ] {
            assert_eq!(chk(&b, h, p, R::Script), Decision::Block, "{h}");
        }
        assert_eq!(
            chk(&b, "https://www.facebook.com/", p, R::Script),
            Decision::Allow
        );
        assert_eq!(
            chk(&b, "https://example.org/", p, R::MainFrame),
            Decision::Allow
        );
    }

    #[test]
    fn builtin_list_allows_duckduckgo_images() {
        // Regression guard for "DuckDuckGo Images is empty": none of the requests the images
        // tab makes may be blocked by the built-in list, as first or third party.
        let b = Blocker::with_builtin_lists();
        let page = Some("https://duckduckgo.com/?q=kek&iax=images&ia=images");
        for (url, t) in [
            (
                "https://duckduckgo.com/?q=kek&iax=images&ia=images",
                R::MainFrame,
            ),
            (
                "https://duckduckgo.com/i.js?l=wt-wt&o=json&q=kek&vqd=4-123&f=,,,,,&p=1",
                R::Xhr,
            ),
            (
                "https://duckduckgo.com/d.js?q=kek&vqd=4-123&l=wt-wt&p=1&s=0",
                R::Script,
            ),
            (
                "https://duckduckgo.com/dist/wpm.images.1a2b3c.js",
                R::Script,
            ),
            ("https://duckduckgo.com/local.js?get_ip=1", R::Xhr),
            (
                "https://external-content.duckduckgo.com/iu/?u=https%3A%2F%2Ftse1.mm.bing.net%2Fth%3Fid%3DOIP.x&f=1&nofb=1",
                R::Image,
            ),
            ("https://tse1.mm.bing.net/th?id=OIP.abc&pid=Api", R::Image),
            ("https://improving.duckduckgo.com/t/iai?7&q=kek", R::Ping),
            ("https://improving.duckduckgo.com/t/iai?7&q=kek", R::Image),
            ("https://links.duckduckgo.com/d.js?q=kek", R::Script),
        ] {
            assert_eq!(chk(&b, url, page, t), Decision::Allow, "{url} ({t:?})");
        }
    }

    #[test]
    fn performance_sanity() {
        let mut list = String::new();
        for i in 0..10_000 {
            match i % 3 {
                0 => list.push_str(&format!("||host{i}.example{}.com^\n", i % 97)),
                1 => list.push_str(&format!("/path{i}/banner*.gif\n")),
                _ => list.push_str(&format!("||h{i}.net/ads/*$third-party\n")),
            }
        }
        let mut b = Blocker::new();
        assert_eq!(b.add_filter_list(&list), 10_000);
        let page = u("https://www.news.org/");
        let urls: Vec<Url> = (0..1000)
            .map(|i| {
                u(&format!(
                    "https://cdn{}.site.org/static/app{i}.js?v=3",
                    i % 7
                ))
            })
            .collect();
        let t = std::time::Instant::now();
        let mut blocked = 0;
        for _ in 0..(if cfg!(debug_assertions) { 1 } else { 100 }) {
            for url in &urls {
                let d = b.check(&RequestInfo {
                    url,
                    initiator: Some(&page),
                    resource_type: R::Script,
                });
                blocked += (d == Decision::Block) as u32;
            }
        }
        assert_eq!(blocked, 0);
        assert!(t.elapsed().as_secs_f32() < 5.0);
        assert_eq!(
            chk(
                &b,
                "https://host3.example3.com/x",
                Some(page.as_ref()),
                R::Script
            ),
            Decision::Block
        );
        assert_eq!(
            chk(&b, "https://z.com/path4/banner1.gif", None, R::Image),
            Decision::Block
        );
    }
}
