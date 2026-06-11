//! `multipart/form-data` (RFC 7578). The parser half is a pure incremental
//! state machine — fed chunks, drained as events, no IO — so the grammar is
//! unit-testable at every chunk straddle and fuzzable in isolation
//! (`fuzz/fuzz_targets/multipart_parse.rs`). The extractor half (Task 7)
//! adapts it to the request body lanes.

// The parser is `pub(crate)` with no non-test consumer yet: it is consumed by
// the Multipart extractor (Task 7) and the fuzz hook (Task 10). Until then the
// only callers are this file's tests, which `dead_code` analysis ignores.
#![allow(dead_code)]

use bytes::{Bytes, BytesMut};

/// Part headers larger than this are rejected (413) — headers are
/// attacker-controlled and have no legitimate reason to be large.
pub(crate) const MAX_PART_HEADER_BYTES: usize = 8 * 1024;
/// More parts than this is rejected (413) — a part-count bomb, not a form.
pub(crate) const MAX_PARTS: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PartMeta {
    pub(crate) name: String,
    pub(crate) filename: Option<String>,
    pub(crate) content_type: Option<String>,
}

pub(crate) enum Event {
    PartHeaders(PartMeta),
    Data(Bytes),
    EndOfPart,
    Done,
}

#[derive(Debug)]
pub(crate) enum ParseError {
    Malformed(&'static str),
    HeadersTooLarge,
    TooManyParts,
}

enum State {
    Preamble,
    AfterBoundary,
    Headers,
    Data,
    Done,
}

pub(crate) struct Parser {
    /// The delimiter as it appears mid-stream: `\r\n--<boundary>`.
    delimiter: Vec<u8>,
    buf: BytesMut,
    state: State,
    parts: usize,
    eof: bool,
    /// How far into `buf` the Headers-state `\r\n\r\n` scan has already looked.
    /// Persisted across `next_event` calls so a header block fed one byte at a
    /// time is scanned once (O(n)) instead of re-scanned from 0 each feed
    /// (O(n²)). Reset to 0 on every transition into Headers and on a hit.
    header_scan_from: usize,
}

impl Parser {
    pub(crate) fn new(boundary: &str) -> Self {
        let mut delimiter = Vec::with_capacity(boundary.len() + 4);
        delimiter.extend_from_slice(b"\r\n--");
        delimiter.extend_from_slice(boundary.as_bytes());
        Self {
            delimiter,
            buf: BytesMut::new(),
            state: State::Preamble,
            parts: 0,
            eof: false,
            header_scan_from: 0,
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// No more input will arrive. After this, `next_event` never returns
    /// `Ok(None)` — every state resolves to an event or a truncation error.
    pub(crate) fn finish(&mut self) {
        self.eof = true;
    }

    /// The next parse event, or `Ok(None)` when more input is needed.
    pub(crate) fn next_event(&mut self) -> std::result::Result<Option<Event>, ParseError> {
        loop {
            match self.state {
                State::Done => return Ok(Some(Event::Done)),
                State::Preamble => {
                    // The FIRST boundary may sit at offset 0 without a leading CRLF.
                    let bare = &self.delimiter[2..];
                    if self.buf.len() >= bare.len() && self.buf[..bare.len()] == *bare {
                        let _ = self.buf.split_to(bare.len());
                        self.state = State::AfterBoundary;
                        continue;
                    }
                    match find(&self.buf, &self.delimiter) {
                        Some(i) => {
                            let _ = self.buf.split_to(i + self.delimiter.len());
                            self.state = State::AfterBoundary;
                        }
                        None => {
                            if self.eof {
                                return Err(ParseError::Malformed("no multipart boundary found"));
                            }
                            // Preamble is discardable; keep only a possible
                            // delimiter prefix at the tail.
                            let keep = (self.delimiter.len() - 1).min(self.buf.len());
                            let cut = self.buf.len() - keep;
                            let _ = self.buf.split_to(cut);
                            return Ok(None);
                        }
                    }
                }
                State::AfterBoundary => {
                    // Past `--boundary`: optional transport padding (SP/HT),
                    // then CRLF (a part follows) or `--` (closing boundary).
                    let mut i = 0;
                    while i < self.buf.len() && (self.buf[i] == b' ' || self.buf[i] == b'\t') {
                        i += 1;
                    }
                    // Cap the padding run BEFORE any need-more-data return so the
                    // buffer cannot grow unboundedly while an attacker streams
                    // spaces after a boundary — the only otherwise-unbounded state
                    // (Preamble/Data are bounded by holdback, Headers by its cap).
                    // Real transport padding is a handful of bytes; reusing the
                    // header cap keeps the constant set minimal.
                    if i > MAX_PART_HEADER_BYTES {
                        return Err(ParseError::Malformed(
                            "excessive padding after multipart boundary",
                        ));
                    }
                    if self.buf.len() < i + 2 {
                        if self.eof {
                            return Err(ParseError::Malformed("truncated multipart boundary line"));
                        }
                        return Ok(None);
                    }
                    if &self.buf[i..i + 2] == b"--" {
                        let _ = self.buf.split_to(i + 2);
                        self.state = State::Done;
                        continue;
                    }
                    if &self.buf[i..i + 2] == b"\r\n" {
                        let _ = self.buf.split_to(i + 2);
                        self.parts += 1;
                        if self.parts > MAX_PARTS {
                            return Err(ParseError::TooManyParts);
                        }
                        // Entering Headers with a fresh buffer window: start the
                        // incremental `\r\n\r\n` scan from the beginning.
                        self.header_scan_from = 0;
                        self.state = State::Headers;
                        continue;
                    }
                    return Err(ParseError::Malformed(
                        "invalid bytes after multipart boundary",
                    ));
                }
                State::Headers => {
                    // Resume the `\r\n\r\n` scan from where the last call stopped.
                    // A 4-byte terminator can only newly complete within the last
                    // 3 bytes of previously scanned input plus the freshly fed
                    // bytes, so back up 3 from the cursor. The buffer is never
                    // consumed while in Headers (split_to happens only on the hit
                    // that leaves this state), so cursor positions stay valid.
                    let start = self.header_scan_from.saturating_sub(3);
                    match find(&self.buf[start..], b"\r\n\r\n").map(|i| i + start) {
                        Some(i) => {
                            let block = self.buf.split_to(i + 4);
                            let meta = parse_part_headers(&block[..i])?;
                            // Reset for the next part's header block.
                            self.header_scan_from = 0;
                            self.state = State::Data;
                            return Ok(Some(Event::PartHeaders(meta)));
                        }
                        None => {
                            if self.buf.len() > MAX_PART_HEADER_BYTES {
                                return Err(ParseError::HeadersTooLarge);
                            }
                            if self.eof {
                                return Err(ParseError::Malformed(
                                    "truncated multipart part headers",
                                ));
                            }
                            // Everything up to here has been scanned; next call
                            // resumes from the new tail.
                            self.header_scan_from = self.buf.len();
                            return Ok(None);
                        }
                    }
                }
                State::Data => match find(&self.buf, &self.delimiter) {
                    Some(0) => {
                        let _ = self.buf.split_to(self.delimiter.len());
                        self.state = State::AfterBoundary;
                        return Ok(Some(Event::EndOfPart));
                    }
                    Some(i) => {
                        let data = self.buf.split_to(i).freeze();
                        return Ok(Some(Event::Data(data)));
                    }
                    None => {
                        // Emit all but a possible delimiter prefix (holdback).
                        let keep = (self.delimiter.len() - 1).min(self.buf.len());
                        let emit = self.buf.len() - keep;
                        if emit > 0 {
                            let data = self.buf.split_to(emit).freeze();
                            return Ok(Some(Event::Data(data)));
                        }
                        if self.eof {
                            return Err(ParseError::Malformed("truncated multipart body"));
                        }
                        return Ok(None);
                    }
                },
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_part_headers(block: &[u8]) -> std::result::Result<PartMeta, ParseError> {
    let text = std::str::from_utf8(block)
        .map_err(|_| ParseError::Malformed("part headers are not valid UTF-8"))?;
    let mut name = None;
    let mut filename = None;
    let mut content_type = None;
    for line in text.split("\r\n").filter(|l| !l.is_empty()) {
        let Some((key, value)) = line.split_once(':') else {
            return Err(ParseError::Malformed("malformed part header line"));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "content-disposition" {
            for param in value.split(';').skip(1) {
                let Some((k, v)) = param.split_once('=') else {
                    continue;
                };
                match k.trim() {
                    "name" => name = Some(unquote(v.trim())),
                    "filename" => filename = Some(unquote(v.trim())),
                    _ => {}
                }
            }
        } else if key == "content-type" {
            content_type = Some(value.to_string());
        }
    }
    Ok(PartMeta {
        name: name.ok_or(ParseError::Malformed("part is missing a form-data name"))?,
        filename,
        content_type,
    })
}

/// RFC 2183 quoted-string: strip surrounding quotes, unescape `\"` and `\\`.
/// Unquoted tokens pass through.
fn unquote(v: &str) -> String {
    match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(q) => {
            let mut out = String::with_capacity(q.len());
            let mut chars = q.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        None => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDARY: &str = "XbOuNdArYx";

    fn fixture() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"--XbOuNdArYx\r\n");
        b.extend_from_slice(b"content-disposition: form-data; name=\"title\"\r\n\r\n");
        b.extend_from_slice(b"hello world\r\n");
        b.extend_from_slice(b"--XbOuNdArYx\r\n");
        b.extend_from_slice(
            b"content-disposition: form-data; name=\"file\"; filename=\"a.csv\"\r\ncontent-type: text/csv\r\n\r\n",
        );
        b.extend_from_slice(b"col\r\n--not-a-boundary\r\nrow2\r\n"); // CRLF-- INSIDE data
        b.extend_from_slice(b"\r\n--XbOuNdArYx--\r\n");
        b
    }

    /// Drives the parser over `input` in `chunk`-byte steps.
    fn run(input: &[u8], chunk: usize) -> (Vec<Vec<u8>>, Vec<PartMeta>) {
        let mut p = Parser::new(BOUNDARY);
        let mut feeds = input.chunks(chunk);
        let mut datas: Vec<Vec<u8>> = Vec::new();
        let mut metas = Vec::new();
        loop {
            match p.next_event().expect("valid fixture") {
                Some(Event::PartHeaders(m)) => {
                    metas.push(m);
                    datas.push(Vec::new());
                }
                Some(Event::Data(d)) => datas.last_mut().unwrap().extend_from_slice(&d),
                Some(Event::EndOfPart) => {}
                Some(Event::Done) => return (datas, metas),
                None => match feeds.next() {
                    Some(c) => p.feed(c),
                    None => p.finish(),
                },
            }
        }
    }

    /// THE invariant: chunking must never change what is parsed. Every chunk
    /// size from 1 byte up exercises every possible boundary straddle.
    #[test]
    fn every_chunking_yields_identical_parts() {
        let input = fixture();
        let (want_data, want_meta) = run(&input, input.len());
        assert_eq!(want_data.len(), 2);
        assert_eq!(want_data[0], b"hello world");
        assert_eq!(
            &want_data[1][..],
            b"col\r\n--not-a-boundary\r\nrow2\r\n".as_slice()
        );
        assert_eq!(want_meta[1].filename.as_deref(), Some("a.csv"));
        assert_eq!(want_meta[1].content_type.as_deref(), Some("text/csv"));
        for chunk in 1..=input.len() {
            let (data, meta) = run(&input, chunk);
            assert_eq!(data, want_data, "chunk size {chunk}");
            // Full meta equality (name/filename/content_type), not just length —
            // chunking must not perturb any parsed header field at any straddle.
            assert_eq!(meta, want_meta, "chunk size {chunk}");
        }
    }

    #[test]
    fn preamble_is_ignored_and_epilogue_is_ignored() {
        let mut input = b"this is preamble junk\r\n".to_vec();
        input.extend_from_slice(&fixture());
        input.extend_from_slice(b"trailing epilogue junk");
        let (data, _) = run(&input, 7);
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], b"hello world");
    }

    #[test]
    fn truncated_input_is_malformed_not_a_hang() {
        let input = fixture();
        for cut in [10, 40, input.len() - 5] {
            let mut p = Parser::new(BOUNDARY);
            p.feed(&input[..cut]);
            p.finish();
            let mut saw_err = false;
            for _ in 0..1000 {
                match p.next_event() {
                    Err(_) => {
                        saw_err = true;
                        break;
                    }
                    Ok(Some(Event::Done)) => break,
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("NeedMore after finish() at cut {cut}"),
                }
            }
            assert!(saw_err, "cut {cut} must error (truncation), not complete");
        }
    }

    /// Data-state truncation where the input ends mid-delimiter: the tail is a
    /// PARTIAL delimiter (`\r\n--XbOuNdArY`, one byte short of the full token),
    /// held back as a possible boundary prefix. On finish() this must surface as
    /// truncation, never silent data loss (the held-back bytes dropped) or a hang.
    #[test]
    fn data_ending_mid_partial_delimiter_is_truncation() {
        let mut input = Vec::new();
        input.extend_from_slice(b"--XbOuNdArYx\r\n");
        input.extend_from_slice(b"content-disposition: form-data; name=\"f\"\r\n\r\n");
        input.extend_from_slice(b"payload");
        // Full delimiter is "\r\n--XbOuNdArYx"; drop the final 'x' so the buffer
        // ends one byte short of a boundary, all of it inside the holdback window.
        input.extend_from_slice(b"\r\n--XbOuNdArY");
        let mut p = Parser::new(BOUNDARY);
        p.feed(&input);
        p.finish();
        // Drain any leading "payload" Data event, then require the truncation error.
        let mut saw_err = None;
        for _ in 0..1000 {
            match p.next_event() {
                Err(e) => {
                    saw_err = Some(e);
                    break;
                }
                Ok(Some(Event::Done)) => panic!("completed despite a truncated trailing delimiter"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("NeedMore after finish()"),
            }
        }
        assert!(
            matches!(
                saw_err,
                Some(ParseError::Malformed("truncated multipart body"))
            ),
            "partial trailing delimiter must be truncated multipart body, got {saw_err:?}"
        );
    }

    /// Important 1 regression: a boundary followed by a flood of SP padding must
    /// be rejected before the buffer can grow past the cap, even while the
    /// 2-byte CRLF/`--` discriminator is still pending (need-more-data path).
    #[test]
    fn padding_after_boundary_is_capped() {
        let mut input = b"--XbOuNdArYx".to_vec();
        input.extend_from_slice(&vec![b' '; 9 * 1024]);
        let mut p = Parser::new(BOUNDARY);
        p.feed(&input);
        // No finish(): the cap must fire on the need-more-data path, not via eof.
        assert!(matches!(
            drive_to_error(&mut p),
            ParseError::Malformed("excessive padding after multipart boundary")
        ));
    }

    #[test]
    fn header_block_over_cap_errors() {
        let mut input = b"--XbOuNdArYx\r\ncontent-disposition: form-data; name=\"x".to_vec();
        input.extend_from_slice(&vec![b'a'; MAX_PART_HEADER_BYTES + 1]);
        let mut p = Parser::new(BOUNDARY);
        p.feed(&input);
        assert!(matches!(
            drive_to_error(&mut p),
            ParseError::HeadersTooLarge
        ));
    }

    #[test]
    fn part_count_over_cap_errors() {
        let mut input = Vec::new();
        for i in 0..=MAX_PARTS {
            input.extend_from_slice(b"--XbOuNdArYx\r\n");
            input.extend_from_slice(
                format!("content-disposition: form-data; name=\"f{i}\"\r\n\r\nx\r\n").as_bytes(),
            );
        }
        input.extend_from_slice(b"--XbOuNdArYx--");
        let mut p = Parser::new(BOUNDARY);
        p.feed(&input);
        p.finish();
        assert!(matches!(drive_to_error(&mut p), ParseError::TooManyParts));
    }

    #[test]
    fn missing_name_is_malformed() {
        let input = b"--XbOuNdArYx\r\ncontent-disposition: form-data\r\n\r\nx\r\n--XbOuNdArYx--";
        let mut p = Parser::new(BOUNDARY);
        p.feed(input);
        p.finish();
        assert!(matches!(drive_to_error(&mut p), ParseError::Malformed(_)));
    }

    #[test]
    fn quoted_filenames_unescape() {
        let input = b"--XbOuNdArYx\r\ncontent-disposition: form-data; name=\"f\"; filename=\"a \\\"b\\\".txt\"\r\n\r\nx\r\n--XbOuNdArYx--";
        let mut p = Parser::new(BOUNDARY);
        p.feed(input);
        p.finish();
        let meta = loop {
            match p.next_event().unwrap() {
                Some(Event::PartHeaders(m)) => break m,
                Some(_) => {}
                None => unreachable!(),
            }
        };
        assert_eq!(meta.filename.as_deref(), Some("a \"b\".txt"));
    }

    /// RFC-degenerate edge: a value ending in an escaped quote (`filename="x\""`).
    /// The naive strip-then-unescape leaves a dangling backslash; this proves the
    /// parser neither panics nor loops, and produces a sane (lossless of `x`) result.
    #[test]
    fn filename_ending_in_escaped_quote_does_not_panic() {
        let input = b"--XbOuNdArYx\r\ncontent-disposition: form-data; name=\"f\"; filename=\"x\\\"\"\r\n\r\nx\r\n--XbOuNdArYx--";
        let mut p = Parser::new(BOUNDARY);
        p.feed(input);
        p.finish();
        let meta = loop {
            match p.next_event().unwrap() {
                Some(Event::PartHeaders(m)) => break m,
                Some(_) => {}
                None => unreachable!(),
            }
        };
        // Whatever the strip yields, it must contain the leading `x` and not panic.
        assert!(meta.filename.as_deref().unwrap().starts_with('x'));
    }

    fn drive_to_error(p: &mut Parser) -> ParseError {
        for _ in 0..100_000 {
            match p.next_event() {
                Err(e) => return e,
                Ok(Some(Event::Done)) => panic!("completed without error"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("NeedMore in drive_to_error"),
            }
        }
        panic!("no error after 100k events");
    }
}
