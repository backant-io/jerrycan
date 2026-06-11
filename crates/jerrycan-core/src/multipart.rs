//! `multipart/form-data` (RFC 7578). The parser half is a pure incremental
//! state machine — fed chunks, drained as events, no IO — so the grammar is
//! unit-testable at every chunk straddle and fuzzable in isolation
//! (`fuzz/fuzz_targets/multipart_parse.rs`). The extractor half (Task 7)
//! adapts it to the request body lanes.

use crate::error::{Error, Result};
use crate::extract::{BodyLane, FromRequest, RequestCtx, StreamLane, map_stream_error};
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

/// Default per-part size cap for `Part::bytes`/`Part::text` (8 MiB). Override
/// per request with [`Multipart::set_part_cap`]. Streamed `chunk()` reads are
/// not capped by this — the route's cumulative `body_limit` governs them.
pub(crate) const DEFAULT_PART_CAP: usize = 8 * 1024 * 1024;

/// Streaming `multipart/form-data` extractor. Parts arrive in wire order and
/// must be consumed sequentially; [`next_part`](Multipart::next_part) discards
/// any unread remainder of the previous part. Requires
/// `content-type: multipart/form-data` with a valid boundary — anything else is
/// `415 JC0415`.
///
/// Single-consumer: the extractor takes ownership of the body, so extracting it
/// twice in one handler is a programming error (500 on stream routes).
pub struct Multipart {
    parser: Parser,
    source: Option<StreamLane>,
    part_cap: usize,
    in_part: bool,
    done: bool,
}

impl FromRequest for Multipart {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let content_type = ctx
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let boundary =
            boundary_from_content_type(content_type).ok_or_else(Error::unsupported_media_type)?;
        let mut parser = Parser::new(&boundary);
        let source = match &mut ctx.body {
            BodyLane::Buffered(bytes) => {
                parser.feed(bytes);
                parser.finish();
                None
            }
            BodyLane::Stream(slot) => Some(
                slot.take()
                    .ok_or_else(|| Error::internal("request body was already consumed"))?,
            ),
        };
        Ok(Multipart {
            parser,
            source,
            part_cap: DEFAULT_PART_CAP,
            in_part: false,
            done: false,
        })
    }
}

impl Multipart {
    /// Per-part byte cap enforced by [`Part::bytes`]/[`Part::text`]
    /// (default 8 MiB).
    pub fn set_part_cap(&mut self, bytes: usize) {
        self.part_cap = bytes;
    }

    /// The next part, or `None` after the closing boundary. Any unread data of
    /// the current part is discarded first.
    pub async fn next_part(&mut self) -> Result<Option<Part<'_>>> {
        if self.done {
            return Ok(None);
        }
        while self.in_part {
            match self.pull_event().await? {
                Event::EndOfPart => self.in_part = false,
                Event::Done => {
                    self.done = true;
                    return Ok(None);
                }
                Event::Data(_) => {}
                Event::PartHeaders(_) => {
                    return Err(Error::internal("multipart parser yielded headers mid-part"));
                }
            }
        }
        match self.pull_event().await? {
            Event::PartHeaders(meta) => {
                self.in_part = true;
                Ok(Some(Part {
                    multipart: self,
                    meta,
                }))
            }
            Event::Done => {
                self.done = true;
                Ok(None)
            }
            Event::Data(_) | Event::EndOfPart => Err(Error::internal(
                "multipart parser yielded data outside a part",
            )),
        }
    }

    /// Drain the next parse event, feeding more body bytes from the stream lane
    /// (or `finish`ing the parser at EOF) whenever the parser needs them.
    async fn pull_event(&mut self) -> Result<Event> {
        loop {
            if let Some(event) = self.parser.next_event().map_err(map_parse_error)? {
                return Ok(event);
            }
            match &mut self.source {
                None => {
                    return Err(Error::internal(
                        "multipart parser stalled after end of input",
                    ));
                }
                Some(stream) => {
                    use http_body_util::BodyExt;
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Ok(data) = frame.into_data() {
                                self.parser.feed(&data);
                            }
                        }
                        Some(Err(e)) => return Err(map_stream_error(e)),
                        None => {
                            self.parser.finish();
                            self.source = None;
                        }
                    }
                }
            }
        }
    }
}

/// One part of a multipart request, borrowed from the [`Multipart`] it came
/// from (parts are sequential — finish one before asking for the next).
pub struct Part<'m> {
    multipart: &'m mut Multipart,
    meta: PartMeta,
}

impl Part<'_> {
    /// The `name` from `content-disposition` (always present — enforced).
    pub fn name(&self) -> &str {
        &self.meta.name
    }
    /// The `filename`, when the part is a file upload.
    pub fn filename(&self) -> Option<&str> {
        self.meta.filename.as_deref()
    }
    /// The part's own `content-type` header, when given.
    pub fn content_type(&self) -> Option<&str> {
        self.meta.content_type.as_deref()
    }

    /// The next chunk of this part's data, or `None` at the part's end.
    /// Chunked reads are bounded by the route's cumulative `body_limit`, not
    /// the per-part cap — use them to process big uploads without buffering.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        if !self.multipart.in_part {
            return Ok(None);
        }
        match self.multipart.pull_event().await? {
            Event::Data(data) => Ok(Some(data)),
            Event::EndOfPart => {
                self.multipart.in_part = false;
                Ok(None)
            }
            Event::Done => {
                self.multipart.in_part = false;
                self.multipart.done = true;
                Ok(None)
            }
            Event::PartHeaders(_) => {
                Err(Error::internal("multipart parser yielded headers mid-part"))
            }
        }
    }

    /// The whole part, buffered — capped at the per-part cap (413 beyond it).
    pub async fn bytes(mut self) -> Result<Bytes> {
        let cap = self.multipart.part_cap;
        let mut out = BytesMut::new();
        while let Some(chunk) = self.chunk().await? {
            if out.len() + chunk.len() > cap {
                return Err(Error::new(
                    http::StatusCode::PAYLOAD_TOO_LARGE,
                    "JC0413",
                    format!("multipart part exceeds the per-part cap of {cap} bytes"),
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    }

    /// The whole part as UTF-8 text (400 on invalid UTF-8).
    pub async fn text(self) -> Result<String> {
        let bytes = self.bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::bad_request("multipart part is not valid UTF-8"))
    }
}

fn map_parse_error(e: ParseError) -> Error {
    match e {
        ParseError::Malformed(what) => {
            Error::bad_request(format!("malformed multipart body: {what}"))
        }
        ParseError::HeadersTooLarge => Error::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "JC0413",
            format!("multipart part headers exceed {MAX_PART_HEADER_BYTES} bytes"),
        ),
        ParseError::TooManyParts => Error::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "JC0413",
            format!("more than {MAX_PARTS} multipart parts"),
        ),
    }
}

/// Extracts and validates the boundary from a `multipart/form-data`
/// content type. RFC 2046 §5.1.1: 1–70 chars from a restricted set.
fn boundary_from_content_type(value: &str) -> Option<String> {
    let mut segments = value.split(';');
    let media_type = segments.next()?.trim();
    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in segments {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = v.trim();
            let boundary = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(v);
            let valid_char = |c: char| c.is_ascii_alphanumeric() || "'()+_,-./:=? ".contains(c);
            if (1..=70).contains(&boundary.len())
                && boundary.chars().all(valid_char)
                && !boundary.ends_with(' ')
            {
                return Some(boundary.to_string());
            }
            return None;
        }
    }
    None
}

/// Fuzzing hook: drives the parser over `input` split at `chunk` bytes until
/// completion or error. Hidden — the fuzz crate is its only consumer.
#[doc(hidden)]
pub fn fuzz_drive(boundary: &str, input: &[u8], chunk: usize) {
    let chunk = chunk.max(1);
    let mut parser = Parser::new(boundary);
    let mut feeds = input.chunks(chunk);
    // The parser must terminate in events linear in the input size: every
    // event either consumes bytes or is the terminal Done/Err. The budget
    // asserts that — a fuzz-discovered livelock fails loudly here.
    //
    // Bound: worst case is chunk=1. Per fed byte the driver does at most two
    // loop turns — an Ok(None) requesting the feed, then one Ok(Some(_)) that
    // consumes >=1 byte (Data is holdback-bounded to >=1 byte; PartHeaders/
    // EndOfPart consume their boundary/header bytes). Ok(None) turns total
    // input.len()+1 (one per chunk + one finish); event turns total
    // <=input.len() (each consumes >=1 byte). So ~3*input.len()+O(1) turns;
    // input.len()*4 + 64 holds with margin and never underflows on empty input.
    let mut budget = input.len() * 4 + 64;
    loop {
        match parser.next_event() {
            Err(_) => return,
            Ok(Some(Event::Done)) => return,
            Ok(Some(_)) => {}
            Ok(None) => match feeds.next() {
                Some(c) => parser.feed(c),
                None => parser.finish(),
            },
        }
        budget -= 1;
        assert!(budget > 0, "parser did not terminate in linear time");
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

    // ----- Extractor tests ---------------------------------------------------

    use crate::prelude::*;

    const FORM_DATA_CT: &str = "multipart/form-data; boundary=XbOuNdArYx";

    /// Collect every part's `(name, byte length)` — the shared handler body for
    /// the buffered/stream parity tests.
    async fn upload(mut mp: Multipart) -> Result<Json<Vec<(String, usize)>>> {
        let mut out = Vec::new();
        while let Some(part) = mp.next_part().await? {
            let name = part.name().to_string();
            let bytes = part.bytes().await?;
            out.push((name, bytes.len()));
        }
        Ok(Json(out))
    }

    #[tokio::test]
    async fn multipart_extracts_parts_on_a_stream_route() {
        let t = App::new()
            .route("/upload", post(upload).stream_body())
            .into_test();
        let res = t
            .post_bytes_with("/upload", &fixture(), &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 200, "body: {}", res.text());
        // "hello world" is 11 bytes; the csv payload (with the in-data CRLF--)
        // is 29 bytes. Stream framing must not perturb either.
        assert_eq!(
            res.json::<Vec<(String, usize)>>(),
            vec![("title".to_string(), 11), ("file".to_string(), 29)]
        );
    }

    #[tokio::test]
    async fn multipart_works_on_buffered_routes_too() {
        // Same handler, NO `.stream_body()`: the buffered lane feeds the parser
        // upfront and must yield the identical parts.
        let t = App::new().route("/upload", post(upload)).into_test();
        let res = t
            .post_bytes_with("/upload", &fixture(), &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 200, "body: {}", res.text());
        assert_eq!(
            res.json::<Vec<(String, usize)>>(),
            vec![("title".to_string(), 11), ("file".to_string(), 29)]
        );
    }

    #[tokio::test]
    async fn wrong_content_type_is_415() {
        let t = App::new().route("/upload", post(upload)).into_test();
        // post_bytes defaults to application/octet-stream — not multipart.
        let res = t.post_bytes("/upload", &fixture()).await;
        assert_eq!(res.status().as_u16(), 415);
        assert!(res.text().contains("JC0415"), "body: {}", res.text());
    }

    #[tokio::test]
    async fn oversized_part_is_413_with_the_cap_message() {
        async fn tiny_cap(mut mp: Multipart) -> Result<Json<usize>> {
            mp.set_part_cap(16);
            let mut count = 0;
            while let Some(part) = mp.next_part().await? {
                let _ = part.bytes().await?; // the second part (29 bytes) trips the cap
                count += 1;
            }
            Ok(Json(count))
        }
        let t = App::new().route("/upload", post(tiny_cap)).into_test();
        let res = t
            .post_bytes_with("/upload", &fixture(), &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 413, "body: {}", res.text());
        assert!(res.text().contains("per-part"), "body: {}", res.text());
    }

    #[tokio::test]
    async fn malformed_multipart_is_400() {
        let t = App::new().route("/upload", post(upload)).into_test();
        let body = b"--XbOuNdArYx\r\ngarbage-without-colon\r\n\r\n";
        let res = t
            .post_bytes_with("/upload", body, &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 400, "body: {}", res.text());
    }

    #[tokio::test]
    async fn next_part_discards_unread_remainder() {
        // Read part 1's name but NOT its data; the next `next_part` must skip the
        // unread remainder and still surface part 2 correctly.
        async fn skip_first(mut mp: Multipart) -> Result<Json<Vec<String>>> {
            let mut names = Vec::new();
            if let Some(part) = mp.next_part().await? {
                names.push(part.name().to_string());
                // deliberately do not read part.bytes()/chunk()
            }
            while let Some(part) = mp.next_part().await? {
                let name = part.name().to_string();
                let data = part.bytes().await?;
                names.push(format!("{name}:{}", data.len()));
            }
            Ok(Json(names))
        }
        let t = App::new()
            .route("/upload", post(skip_first).stream_body())
            .into_test();
        let res = t
            .post_bytes_with("/upload", &fixture(), &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 200, "body: {}", res.text());
        assert_eq!(
            res.json::<Vec<String>>(),
            vec!["title".to_string(), "file:29".to_string()]
        );
    }

    #[tokio::test]
    async fn chunked_reads_stream_without_part_cap() {
        // A single part larger than a tiny per-part cap, read via `chunk()`:
        // chunk() is governed by the route body_limit, NOT the per-part cap, so
        // it must succeed even though `bytes()` would 413 at the same cap.
        async fn stream_part(mut mp: Multipart) -> Result<Json<usize>> {
            mp.set_part_cap(4); // far below the part's real size
            let mut total = 0;
            while let Some(mut part) = mp.next_part().await? {
                while let Some(chunk) = part.chunk().await? {
                    total += chunk.len();
                }
            }
            Ok(Json(total))
        }
        // One part whose data is 200 bytes of 'z' — well over the 4-byte cap.
        let payload = "z".repeat(200);
        let mut body = Vec::new();
        body.extend_from_slice(b"--XbOuNdArYx\r\n");
        body.extend_from_slice(b"content-disposition: form-data; name=\"big\"\r\n\r\n");
        body.extend_from_slice(payload.as_bytes());
        body.extend_from_slice(b"\r\n--XbOuNdArYx--\r\n");
        let t = App::new()
            .route(
                "/upload",
                post(stream_part).stream_body().body_limit(64 * 1024),
            )
            .into_test();
        let res = t
            .post_bytes_with("/upload", &body, &[("content-type", FORM_DATA_CT)])
            .await;
        assert_eq!(res.status().as_u16(), 200, "body: {}", res.text());
        assert_eq!(res.json::<usize>(), 200);
    }

    #[tokio::test]
    async fn multipart_rejects_a_task_context_with_jc1003() {
        // HTTP-coupled extractor inside a task context must reject before reading
        // anything — mirrors the Headers/Json guard.
        use crate::dep::DepEnv;
        use crate::dep::DepResolver;
        use std::sync::Arc;
        let req = http::Request::builder()
            .uri("/")
            .header(http::header::CONTENT_TYPE, FORM_DATA_CT)
            .body(())
            .unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            Bytes::new(),
            DepResolver::new(Arc::new(DepEnv::default()), Default::default()),
        );
        ctx.is_task = true;
        let err = Multipart::from_request(&mut ctx).await.err().unwrap();
        assert_eq!(err.code(), "JC1003");
        assert_eq!(err.status().as_u16(), 500);
    }

    // ----- boundary_from_content_type unit tests -----------------------------

    #[test]
    fn boundary_quoted_value_is_unquoted() {
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=\"abc123\"").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn boundary_over_70_chars_is_rejected() {
        let long = "x".repeat(71);
        let ct = format!("multipart/form-data; boundary={long}");
        assert_eq!(boundary_from_content_type(&ct), None);
        // Exactly 70 is the allowed maximum.
        let ok = "y".repeat(70);
        let ct = format!("multipart/form-data; boundary={ok}");
        assert_eq!(
            boundary_from_content_type(&ct).as_deref(),
            Some(ok.as_str())
        );
    }

    #[test]
    fn boundary_empty_is_rejected() {
        // An empty boundary makes the parser grammar degenerate — must be None.
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary="),
            None
        );
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=\"\""),
            None
        );
    }

    #[test]
    fn boundary_media_type_is_case_insensitive() {
        assert_eq!(
            boundary_from_content_type("MULTIPART/FORM-DATA; BOUNDARY=x").as_deref(),
            Some("x")
        );
    }

    #[test]
    fn boundary_missing_is_none() {
        assert_eq!(boundary_from_content_type("multipart/form-data"), None);
        // A non-multipart media type is also None (→ 415 at the call site).
        assert_eq!(
            boundary_from_content_type("application/json; boundary=x"),
            None
        );
    }

    #[test]
    fn boundary_invalid_chars_are_rejected() {
        // `*` is outside the RFC 2046 §5.1.1 restricted set.
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=a*b"),
            None
        );
    }

    #[test]
    fn boundary_trailing_space_is_rejected() {
        // A space is a valid bchars char but may not be the LAST char.
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=\"abc \""),
            None
        );
    }
}
