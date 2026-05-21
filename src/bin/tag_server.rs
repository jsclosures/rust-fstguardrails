//! HTTP server exposing the tagger, written directly on
//! `std::net::TcpListener` — mirrors the JDK `com.sun.net.httpserver`
//! style used by the Java `App.java` reference.
//!
//! Usage:
//!     tag-server [dictionary.tsv] [addr]
//!     # addr defaults to 0.0.0.0:$PORT (PORT defaults to 8080)
//!
//! If `DATA` is set, every `*.csv` in that directory is loaded and the
//! optional dictionary argument is ignored.
//!
//! Endpoints (Java parity):
//!   GET  /health
//!     -> {"status":"ok"}
//!   GET  /tag?text=...&format=simple|solr
//!     -> simple: {"totaltime":N,"text":"...","docs":[{start,end,surface,id,type,output}]}
//!     -> solr:   {"totalTime":N,"response":{"numFound":N,"start":0,"docs":[{id,name:[],type}]},
//!                 "tags":[{"startOffset":N,"endOffset":N,"ids":[...]}]}
//!   POST /tag    body: {"text":"...","format":"simple|solr"}  (format optional)
//!     -> same shapes; `?format=` on the query string overrides body

use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use lume::{Tag, Tagger};

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();

    let (tagger, source) = match Tagger::from_env() {
        Ok(Some(t)) => {
            let src = format!(
                "DATA={} ({} records, kinds: {})",
                env::var("DATA").unwrap_or_default(),
                t.record_count(),
                t.kinds().join(", ")
            );
            (Arc::new(t), src)
        }
        Ok(None) => {
            if args.is_empty() {
                eprintln!(
                    "usage: tag-server <dictionary.tsv> [addr]   (or set DATA=<csv dir>)"
                );
                process::exit(2);
            }
            let dict = args.remove(0);
            match Tagger::from_tsv_file(&dict) {
                Ok(t) => (Arc::new(t), dict),
                Err(e) => {
                    eprintln!("failed to load dictionary: {e}");
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("failed to load DATA dir: {e}");
            process::exit(1);
        }
    };

    let port = env::var("PORT").ok().unwrap_or_else(|| "8080".to_string());
    let addr = args
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("0.0.0.0:{port}"));

    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            process::exit(1);
        }
    };
    eprintln!(
        "tag-server listening on http://{addr}  (source: {source})"
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tagger = Arc::clone(&tagger);
                thread::spawn(move || {
                    if let Err(e) = handle(stream, &tagger) {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

fn handle(stream: TcpStream, tagger: &Tagger) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.trim_end().split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => write_response(
            &mut writer,
            200,
            "application/json",
            br#"{"status":"ok"}"#,
        ),
        ("GET", "/tag") => {
            let text = query_param(&query, "text").unwrap_or_default();
            let format = query_param(&query, "format");
            handle_tag(&mut writer, tagger, &text, format.as_deref())
        }
        ("POST", "/tag") => {
            let mut buf = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut buf)?;
            }
            let body = String::from_utf8_lossy(&buf).into_owned();
            let text = json_string_field(&body, "text").unwrap_or_default();
            // Body's format is the default; ?format= on URL overrides.
            let body_format = json_string_field(&body, "format");
            let url_format = query_param(&query, "format");
            let format = url_format.or(body_format);
            handle_tag(&mut writer, tagger, &text, format.as_deref())
        }
        // Java parity: known paths reject unsupported methods with 405.
        (_, "/tag") | (_, "/health") => write_response(
            &mut writer,
            405,
            "application/json",
            br#"{"error":"method not allowed"}"#,
        ),
        _ => write_response(
            &mut writer,
            404,
            "application/json",
            br#"{"error":"not found"}"#,
        ),
    }
}

fn handle_tag(
    writer: &mut TcpStream,
    tagger: &Tagger,
    text: &str,
    format: Option<&str>,
) -> std::io::Result<()> {
    if text.trim().is_empty() {
        return write_response(
            writer,
            400,
            "application/json",
            br#"{"error":"provide ?text= (GET) or {\"text\":\"...\"} (POST)"}"#,
        );
    }
    let fmt = format.unwrap_or("simple").to_ascii_lowercase();
    if fmt != "simple" && fmt != "solr" {
        let body = format!(
            r#"{{"error":"unknown format '{}' — supported: simple, solr"}}"#,
            json_escape(&fmt)
        );
        return write_response(writer, 400, "application/json", body.as_bytes());
    }

    let started = Instant::now();
    let tags = tagger.tag(text);
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let body = if fmt == "solr" {
        to_solr_json(text, &tags, elapsed_ms)
    } else {
        to_simple_json(text, &tags, elapsed_ms)
    };
    write_response(writer, 200, "application/json", body.as_bytes())
}

fn write_response(
    w: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    w.write_all(header.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}

// ─── JSON serializers (parity with App.java toJson / toSolrJson) ─────────

fn to_simple_json(text: &str, tags: &[Tag], elapsed_ms: u64) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{{\n  \"totaltime\": {elapsed_ms},\n  \"text\": \"{}\",\n  \"docs\": ",
        json_escape(text)
    ));
    if tags.is_empty() {
        s.push_str("[]");
    } else {
        s.push_str("[\n");
        for (i, t) in tags.iter().enumerate() {
            s.push_str(&format!(
                "    {{\"start\":{},\"end\":{},\"surface\":\"{}\",\"id\":\"{}\",\"type\":\"{}\",\"output\":\"{}\"}}",
                t.start,
                t.end,
                json_escape(&t.surface),
                json_escape(&t.id),
                json_escape(&t.kind),
                json_escape(&t.output),
            ));
            if i < tags.len() - 1 {
                s.push(',');
            }
            s.push('\n');
        }
        s.push_str("  ]");
    }
    s.push_str("\n}");
    s
}

fn to_solr_json(_text: &str, tags: &[Tag], elapsed_ms: u64) -> String {
    // response.docs: one entry per unique id; name = ordered unique surfaces
    let mut names_by_id: Vec<(String, Vec<String>)> = Vec::new();
    let mut type_by_id: Vec<(String, String)> = Vec::new();
    for t in tags {
        if let Some(entry) = names_by_id.iter_mut().find(|(id, _)| id == &t.id) {
            if !entry.1.iter().any(|n| n == &t.surface) {
                entry.1.push(t.surface.clone());
            }
        } else {
            names_by_id.push((t.id.clone(), vec![t.surface.clone()]));
            type_by_id.push((t.id.clone(), t.kind.clone()));
        }
    }

    // tags: grouped by (start,end); ids in insertion order
    let mut spans: BTreeMap<(usize, usize), Vec<String>> = BTreeMap::new();
    for t in tags {
        let v = spans.entry((t.start, t.end)).or_default();
        if !v.contains(&t.id) {
            v.push(t.id.clone());
        }
    }

    let mut s = String::new();
    s.push_str(&format!(
        "{{\n  \"totalTime\": {elapsed_ms},\n  \"response\": {{\n    \"numFound\": {},\n    \"start\": 0,\n    \"docs\": [",
        names_by_id.len()
    ));
    for (i, (id, names)) in names_by_id.iter().enumerate() {
        s.push_str(if i == 0 { "\n" } else { ",\n" });
        let kind = type_by_id
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let names_json: Vec<String> = names
            .iter()
            .map(|n| format!("\"{}\"", json_escape(n)))
            .collect();
        s.push_str(&format!(
            "      {{\"id\":\"{}\",\"name\":[{}],\"type\":\"{}\"}}",
            json_escape(id),
            names_json.join(","),
            json_escape(kind),
        ));
    }
    if !names_by_id.is_empty() {
        s.push_str("\n    ");
    }
    s.push_str("]\n  },\n  \"tags\": [");
    for (i, ((start, end), ids)) in spans.iter().enumerate() {
        s.push_str(if i == 0 { "\n" } else { ",\n" });
        let ids_json: Vec<String> = ids
            .iter()
            .map(|id| format!("\"{}\"", json_escape(id)))
            .collect();
        s.push_str(&format!(
            "    {{\"startOffset\":{start},\"endOffset\":{end},\"ids\":[{}]}}",
            ids_json.join(",")
        ));
    }
    if !spans.is_empty() {
        s.push_str("\n  ");
    }
    s.push_str("]\n}");
    s
}

// ─── Tiny helpers ───────────────────────────────────────────────────────

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(url_decode(v));
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract a top-level JSON string field by name, with basic escape
/// unwrapping. Matches App.java's regex-based extractor in scope.
fn json_string_field(json: &str, key: &str) -> Option<String> {
    let bytes = json.as_bytes();
    let needle = format!("\"{key}\"");
    let nb = needle.as_bytes();
    let mut i = 0;
    while i + nb.len() <= bytes.len() {
        if &bytes[i..i + nb.len()] == nb {
            let mut j = i + nb.len();
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b':' {
                i += 1;
                continue;
            }
            j += 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'"' {
                i += 1;
                continue;
            }
            j += 1;
            let mut out = String::new();
            while j < bytes.len() {
                let c = bytes[j];
                if c == b'\\' && j + 1 < bytes.len() {
                    let esc = bytes[j + 1];
                    out.push(match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'/' => '/',
                        _ => esc as char,
                    });
                    j += 2;
                } else if c == b'"' {
                    return Some(out);
                } else {
                    out.push(c as char);
                    j += 1;
                }
            }
            return None;
        }
        i += 1;
    }
    None
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
