//! Bookmarks: a tree of folders and bookmarks with three roots (toolbar, other, mobile-ish
//! "unsorted"). Persisted as JSON; import/export in the Netscape HTML format every browser
//! understands.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::storage::{read_json, write_json};
use crate::util::{html_escape, html_unescape, match_score};

pub type BookmarkId = u64;

pub const TOOLBAR_ID: BookmarkId = 1;
pub const OTHER_ID: BookmarkId = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bookmark {
    pub id: BookmarkId,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub added_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BookmarkNode {
    Bookmark(Bookmark),
    Folder {
        id: BookmarkId,
        title: String,
        #[serde(default)]
        children: Vec<BookmarkNode>,
        #[serde(default)]
        added_ms: i64,
    },
}

impl BookmarkNode {
    pub fn id(&self) -> BookmarkId {
        match self {
            Self::Bookmark(b) => b.id,
            Self::Folder { id, .. } => *id,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Bookmark(b) => &b.title,
            Self::Folder { title, .. } => title,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self, Self::Folder { .. })
    }

    pub fn children(&self) -> &[BookmarkNode] {
        match self {
            Self::Folder { children, .. } => children,
            _ => &[],
        }
    }

    fn walk<'a>(&'a self, out: &mut Vec<&'a Bookmark>) {
        match self {
            Self::Bookmark(b) => out.push(b),
            Self::Folder { children, .. } => children.iter().for_each(|c| c.walk(out)),
        }
    }

    fn find(&self, id: BookmarkId) -> Option<&BookmarkNode> {
        if self.id() == id {
            return Some(self);
        }
        self.children().iter().find_map(|c| c.find(id))
    }

    fn find_mut(&mut self, id: BookmarkId) -> Option<&mut BookmarkNode> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Self::Folder { children, .. } => children.iter_mut().find_map(|c| c.find_mut(id)),
            _ => None,
        }
    }

    /// Remove node `id` from the subtree, returning it.
    fn take(&mut self, id: BookmarkId) -> Option<BookmarkNode> {
        let Self::Folder { children, .. } = self else {
            return None;
        };
        if let Some(pos) = children.iter().position(|c| c.id() == id) {
            return Some(children.remove(pos));
        }
        children.iter_mut().find_map(|c| c.take(id))
    }

    fn contains(&self, id: BookmarkId) -> bool {
        self.find(id).is_some()
    }

    fn max_id(&self) -> BookmarkId {
        self.children()
            .iter()
            .map(|c| c.max_id())
            .fold(self.id(), BookmarkId::max)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BookmarkFile {
    version: u32,
    toolbar: BookmarkNode,
    other: BookmarkNode,
}

impl Default for BookmarkFile {
    fn default() -> Self {
        Self {
            version: 1,
            toolbar: BookmarkNode::Folder {
                id: TOOLBAR_ID,
                title: "Bookmarks bar".into(),
                children: vec![],
                added_ms: 0,
            },
            other: BookmarkNode::Folder {
                id: OTHER_ID,
                title: "Other bookmarks".into(),
                children: vec![],
                added_ms: 0,
            },
        }
    }
}

/// The bookmark store. All mutating methods persist immediately (writes are tiny).
pub struct BookmarkStore {
    path: Option<PathBuf>,
    data: BookmarkFile,
    next_id: BookmarkId,
    /// Monotonic counter bumped on every change so UIs can cheaply detect updates.
    revision: u64,
}

impl BookmarkStore {
    pub fn open(path: PathBuf) -> Self {
        let data = read_json(&path).ok().flatten().unwrap_or_default();
        let mut store = Self {
            path: Some(path),
            data,
            next_id: 0,
            revision: 0,
        };
        store.next_id = store
            .data
            .toolbar
            .max_id()
            .max(store.data.other.max_id())
            .max(OTHER_ID)
            + 1;
        store
    }

    pub fn in_memory() -> Self {
        Self {
            path: None,
            data: BookmarkFile::default(),
            next_id: OTHER_ID + 1,
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision += 1;
        if let Some(path) = &self.path
            && let Err(e) = write_json(path, &self.data)
        {
            log::error!("Failed to save bookmarks: {e}");
        }
    }

    fn alloc_id(&mut self) -> BookmarkId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn toolbar(&self) -> &BookmarkNode {
        &self.data.toolbar
    }

    pub fn other(&self) -> &BookmarkNode {
        &self.data.other
    }

    pub fn roots(&self) -> [&BookmarkNode; 2] {
        [&self.data.toolbar, &self.data.other]
    }

    pub fn get(&self, id: BookmarkId) -> Option<&BookmarkNode> {
        self.data
            .toolbar
            .find(id)
            .or_else(|| self.data.other.find(id))
    }

    fn get_mut(&mut self, id: BookmarkId) -> Option<&mut BookmarkNode> {
        if self.data.toolbar.contains(id) {
            self.data.toolbar.find_mut(id)
        } else {
            self.data.other.find_mut(id)
        }
    }

    pub fn all(&self) -> Vec<&Bookmark> {
        let mut out = Vec::new();
        self.data.toolbar.walk(&mut out);
        self.data.other.walk(&mut out);
        out
    }

    pub fn len(&self) -> usize {
        self.all().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn find_by_url(&self, url: &str) -> Option<&Bookmark> {
        self.all().into_iter().find(|b| b.url == url)
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.find_by_url(url).is_some()
    }

    /// Add a bookmark to `parent` (a folder id), returning its id.
    pub fn add(
        &mut self,
        parent: BookmarkId,
        title: &str,
        url: &str,
    ) -> anyhow::Result<BookmarkId> {
        let id = self.alloc_id();
        let node = BookmarkNode::Bookmark(Bookmark {
            id,
            title: if title.trim().is_empty() {
                url.to_string()
            } else {
                title.trim().to_string()
            },
            url: url.to_string(),
            tags: vec![],
            added_ms: crate::now_ms(),
        });
        self.insert(parent, node, None)?;
        self.changed();
        Ok(id)
    }

    pub fn add_folder(&mut self, parent: BookmarkId, title: &str) -> anyhow::Result<BookmarkId> {
        let id = self.alloc_id();
        let node = BookmarkNode::Folder {
            id,
            title: title.trim().to_string(),
            children: vec![],
            added_ms: crate::now_ms(),
        };
        self.insert(parent, node, None)?;
        self.changed();
        Ok(id)
    }

    fn insert(
        &mut self,
        parent: BookmarkId,
        node: BookmarkNode,
        index: Option<usize>,
    ) -> anyhow::Result<()> {
        match self.get_mut(parent) {
            Some(BookmarkNode::Folder { children, .. }) => {
                let idx = index.unwrap_or(children.len()).min(children.len());
                children.insert(idx, node);
                Ok(())
            }
            _ => anyhow::bail!("parent {parent} is not a folder"),
        }
    }

    /// Toggle a bookmark for `url`: removes it if present, otherwise adds it to the
    /// toolbar. Returns true if the page is now bookmarked.
    pub fn toggle(&mut self, title: &str, url: &str) -> bool {
        if let Some(id) = self.find_by_url(url).map(|b| b.id) {
            let _ = self.remove(id);
            false
        } else {
            self.add(TOOLBAR_ID, title, url).is_ok()
        }
    }

    pub fn remove(&mut self, id: BookmarkId) -> anyhow::Result<BookmarkNode> {
        anyhow::ensure!(
            id != TOOLBAR_ID && id != OTHER_ID,
            "cannot remove a root folder"
        );
        let node = self
            .data
            .toolbar
            .take(id)
            .or_else(|| self.data.other.take(id))
            .ok_or_else(|| anyhow::anyhow!("no bookmark {id}"))?;
        self.changed();
        Ok(node)
    }

    pub fn rename(&mut self, id: BookmarkId, new_title: &str) -> anyhow::Result<()> {
        match self.get_mut(id) {
            Some(BookmarkNode::Bookmark(b)) => b.title = new_title.to_string(),
            Some(BookmarkNode::Folder { title, .. }) => *title = new_title.to_string(),
            None => anyhow::bail!("no bookmark {id}"),
        }
        self.changed();
        Ok(())
    }

    pub fn update(
        &mut self,
        id: BookmarkId,
        title: &str,
        url: &str,
        tags: Vec<String>,
    ) -> anyhow::Result<()> {
        match self.get_mut(id) {
            Some(BookmarkNode::Bookmark(b)) => {
                b.title = title.to_string();
                b.url = url.to_string();
                b.tags = tags;
            }
            _ => anyhow::bail!("no bookmark {id}"),
        }
        self.changed();
        Ok(())
    }

    /// Move node `id` into folder `new_parent` at `index` (append if None).
    pub fn move_to(
        &mut self,
        id: BookmarkId,
        new_parent: BookmarkId,
        index: Option<usize>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            id != TOOLBAR_ID && id != OTHER_ID,
            "cannot move a root folder"
        );
        if let Some(node) = self.get(id) {
            anyhow::ensure!(
                !node.contains(new_parent),
                "cannot move a folder into itself"
            );
        }
        anyhow::ensure!(
            matches!(self.get(new_parent), Some(BookmarkNode::Folder { .. })),
            "target is not a folder"
        );
        let node = self
            .data
            .toolbar
            .take(id)
            .or_else(|| self.data.other.take(id))
            .ok_or_else(|| anyhow::anyhow!("no bookmark {id}"))?;
        self.insert(new_parent, node, index)?;
        self.changed();
        Ok(())
    }

    /// All folders as (id, indented path) pairs, for "choose folder" pickers.
    pub fn folders(&self) -> Vec<(BookmarkId, String)> {
        fn rec(node: &BookmarkNode, prefix: &str, out: &mut Vec<(BookmarkId, String)>) {
            if let BookmarkNode::Folder {
                id,
                title,
                children,
                ..
            } = node
            {
                let path = if prefix.is_empty() {
                    title.clone()
                } else {
                    format!("{prefix} / {title}")
                };
                out.push((*id, path.clone()));
                children.iter().for_each(|c| rec(c, &path, out));
            }
        }
        let mut out = Vec::new();
        rec(&self.data.toolbar, "", &mut out);
        rec(&self.data.other, "", &mut out);
        out
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(u32, &Bookmark)> {
        let mut hits: Vec<_> = self
            .all()
            .into_iter()
            .filter_map(|b| {
                let hay = format!("{} {} {}", b.title, b.url, b.tags.join(" "));
                match_score(query, &hay).map(|s| (s, b))
            })
            .collect();
        hits.sort_by_key(|b| std::cmp::Reverse(b.0));
        hits.truncate(limit);
        hits
    }

    /// Export everything in Netscape bookmark file format.
    pub fn export_html(&self) -> String {
        fn rec(node: &BookmarkNode, depth: usize, out: &mut String, toolbar: bool) {
            let pad = "    ".repeat(depth);
            match node {
                BookmarkNode::Bookmark(b) => {
                    out.push_str(&format!(
                        "{pad}<DT><A HREF=\"{}\" ADD_DATE=\"{}\">{}</A>\n",
                        html_escape(&b.url),
                        b.added_ms / 1000,
                        html_escape(&b.title)
                    ));
                }
                BookmarkNode::Folder {
                    title,
                    children,
                    added_ms,
                    ..
                } => {
                    let extra = if toolbar {
                        " PERSONAL_TOOLBAR_FOLDER=\"true\""
                    } else {
                        ""
                    };
                    out.push_str(&format!(
                        "{pad}<DT><H3 ADD_DATE=\"{}\"{extra}>{}</H3>\n{pad}<DL><p>\n",
                        added_ms / 1000,
                        html_escape(title)
                    ));
                    for c in children {
                        rec(c, depth + 1, out, false);
                    }
                    out.push_str(&format!("{pad}</DL><p>\n"));
                }
            }
        }
        let mut out = String::from(
            "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
        );
        rec(&self.data.toolbar, 1, &mut out, true);
        for c in self.data.other.children() {
            rec(c, 1, &mut out, false);
        }
        out.push_str("</DL><p>\n");
        out
    }

    /// Import a Netscape-format HTML bookmark file into a new folder under "Other".
    /// Returns the number of bookmarks imported.
    pub fn import_html(&mut self, html: &str, folder_title: &str) -> anyhow::Result<usize> {
        let root = self.add_folder(OTHER_ID, folder_title)?;
        let mut stack = vec![root];
        let mut pending_folder: Option<String> = None;
        let mut count = 0;
        let lower = html.to_ascii_lowercase();
        let mut i = 0;
        while let Some(rel) = lower[i..].find('<') {
            let start = i + rel;
            let end = match lower[start..].find('>') {
                Some(e) => start + e + 1,
                None => break,
            };
            let tag = &lower[start..end];
            if tag.starts_with("<h3") {
                let close = lower[end..].find("</h3").map(|e| end + e).unwrap_or(end);
                pending_folder = Some(html_unescape(html[end..close].trim()));
                i = close;
            } else if tag.starts_with("<dl") {
                let title = pending_folder.take();
                if let Some(title) = title {
                    let parent = *stack.last().unwrap();
                    let id = self.add_folder(parent, &title)?;
                    stack.push(id);
                } else {
                    // The outermost <DL> maps onto our import root.
                    stack.push(*stack.last().unwrap());
                }
                i = end;
            } else if tag.starts_with("</dl") {
                if stack.len() > 1 {
                    stack.pop();
                }
                i = end;
            } else if tag.starts_with("<a ") {
                let raw_tag = &html[start..end];
                let href = attr(raw_tag, "href").unwrap_or_default();
                let close = lower[end..].find("</a").map(|e| end + e).unwrap_or(end);
                let title = html_unescape(html[end..close].trim());
                if !href.is_empty()
                    && !href.starts_with("javascript:")
                    && !href.starts_with("place:")
                {
                    let id = self.alloc_id();
                    let node = BookmarkNode::Bookmark(Bookmark {
                        id,
                        title: if title.is_empty() {
                            href.clone()
                        } else {
                            title
                        },
                        url: href,
                        tags: attr(raw_tag, "tags")
                            .map(|t| {
                                t.split(',')
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty())
                                    .collect()
                            })
                            .unwrap_or_default(),
                        added_ms: attr(raw_tag, "add_date")
                            .and_then(|d| d.parse::<i64>().ok())
                            .map(|s| s * 1000)
                            .unwrap_or_else(crate::now_ms),
                    });
                    self.insert(*stack.last().unwrap(), node, None)?;
                    count += 1;
                }
                i = close;
            } else {
                i = end;
            }
        }
        self.changed();
        Ok(count)
    }

    /// Import a list of (folder path, title, url) entries, used by browser importers.
    pub fn import_entries(
        &mut self,
        folder_title: &str,
        entries: &[(Vec<String>, String, String)],
    ) -> anyhow::Result<usize> {
        let root = self.add_folder(OTHER_ID, folder_title)?;
        let mut folder_ids: std::collections::HashMap<Vec<String>, BookmarkId> = Default::default();
        for (path, title, url) in entries {
            let mut parent = root;
            for depth in 1..=path.len() {
                let key = path[..depth].to_vec();
                parent = match folder_ids.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = self.add_folder(parent, &path[depth - 1])?;
                        folder_ids.insert(key, id);
                        id
                    }
                };
            }
            let id = self.alloc_id();
            self.insert(
                parent,
                BookmarkNode::Bookmark(Bookmark {
                    id,
                    title: title.clone(),
                    url: url.clone(),
                    tags: vec![],
                    added_ms: crate::now_ms(),
                }),
                None,
            )?;
        }
        self.changed();
        Ok(entries.len())
    }
}

/// Extract an attribute value (case-insensitive name) from a raw HTML tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(&needle) {
        let pos = search_from + pos;
        // must be preceded by whitespace to avoid matching e.g. "data-href="
        if pos > 0 && !lower.as_bytes()[pos - 1].is_ascii_whitespace() {
            search_from = pos + needle.len();
            continue;
        }
        let rest = &tag[pos + needle.len()..];
        let value = if let Some(r) = rest.strip_prefix('"') {
            r.split('"').next()
        } else if let Some(r) = rest.strip_prefix('\'') {
            r.split('\'').next()
        } else {
            rest.split(|c: char| c.is_whitespace() || c == '>').next()
        };
        return value.map(html_unescape);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        let mut s = BookmarkStore::open(path.clone());
        let f = s.add_folder(TOOLBAR_ID, "Dev").unwrap();
        let b = s.add(f, "Servo", "https://servo.org").unwrap();
        s.add(TOOLBAR_ID, "", "https://rust-lang.org").unwrap();
        assert!(s.is_bookmarked("https://servo.org"));
        assert!(s.move_to(f, f, None).is_err());
        s.move_to(b, OTHER_ID, Some(0)).unwrap();
        assert_eq!(s.other().children()[0].id(), b);
        assert!(!s.toggle("x", "https://servo.org"));
        assert!(s.toggle("x", "https://servo.org"));
        let n = s.len();
        drop(s);
        let s = BookmarkStore::open(path);
        assert_eq!(s.len(), n);
        assert_eq!(
            s.find_by_url("https://rust-lang.org").unwrap().title,
            "https://rust-lang.org"
        );
        assert!(s.next_id > b);
        assert_eq!(s.search("rust", 5).len(), 1);
        assert_eq!(s.folders().len(), 3);
    }

    #[test]
    fn html_roundtrip() {
        let mut s = BookmarkStore::in_memory();
        let f = s.add_folder(TOOLBAR_ID, "A & B").unwrap();
        s.add(f, "Tom's <page>", "https://example.com/?a=1&b=2")
            .unwrap();
        s.add(OTHER_ID, "Other", "https://other.example").unwrap();
        let html = s.export_html();
        let mut t = BookmarkStore::in_memory();
        let n = t.import_html(&html, "Imported").unwrap();
        assert_eq!(n, 2);
        let b = t.find_by_url("https://example.com/?a=1&b=2").unwrap();
        assert_eq!(b.title, "Tom's <page>");
        let folders: Vec<_> = t.folders().into_iter().map(|f| f.1).collect();
        assert!(folders.iter().any(|f| f.ends_with("A & B")), "{folders:?}");
    }

    #[test]
    fn import_entries() {
        let mut s = BookmarkStore::in_memory();
        s.import_entries(
            "From Chrome",
            &[
                (vec!["x".into(), "y".into()], "t".into(), "https://a".into()),
                (vec!["x".into()], "t2".into(), "https://b".into()),
            ],
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.folders().len(), 5);
    }
}
