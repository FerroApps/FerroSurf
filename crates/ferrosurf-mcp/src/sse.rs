//! Incremental Server-Sent Events parser.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    has_fields: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; returns every event completed by this chunk.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            self.line(&line, &mut out);
        }
        out
    }

    /// Flush a trailing, unterminated event at end of stream.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            let line = line.trim_end_matches('\r').to_string();
            self.line(&line, &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => {
                self.event = Some(value.to_string());
                self.has_fields = true;
            }
            "data" => {
                self.data.push(value.to_string());
                self.has_fields = true;
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.has_fields {
            out.push(SseEvent {
                event: self.event.take(),
                data: self.data.join("\n"),
            });
        }
        self.event = None;
        self.data.clear();
        self.has_fields = false;
    }
}

/// Parse a complete SSE body.
pub fn parse_all(body: &str) -> Vec<SseEvent> {
    let mut p = SseParser::new();
    let mut ev = p.push(body.as_bytes());
    ev.extend(p.finish());
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_and_multiline() {
        let ev = parse_all("event: message\ndata: {\"a\":1}\n\n: comment\ndata: x\ndata: y\n\n");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].event.as_deref(), Some("message"));
        assert_eq!(ev[0].data, "{\"a\":1}");
        assert_eq!(ev[1].data, "x\ny");
    }

    #[test]
    fn crlf_and_chunk_splits() {
        let mut p = SseParser::new();
        let body = "data: héllo\r\n\r\ndata: two\r\n\r\n".as_bytes();
        let mut ev = Vec::new();
        for b in body.chunks(3) {
            ev.extend(p.push(b));
        }
        assert_eq!(
            ev.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(),
            ["héllo", "two"]
        );
    }

    #[test]
    fn trailing_event_flushed() {
        let ev = parse_all("data: last");
        assert_eq!(
            ev,
            vec![SseEvent {
                event: None,
                data: "last".into()
            }]
        );
    }
}
