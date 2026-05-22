use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::bm25::{parse_markdown, Bm25Index, Bm25Params, SearchVariant, Section};
use crate::spelling::SpellIndex;
use crate::{tokenize, Tagger};
use crate::semantic_mesh::MarkovChain;
use serde::{Deserialize, Serialize};

// JSON-RPC Structures
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
    id: serde_json::Value,
}


#[derive(Serialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: serde_json::Value,
}

#[derive(Serialize, Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// MCP Specific Structs
#[derive(Deserialize, Debug)]
struct ToolCallParams {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Serialize, Debug)]
struct McpContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Serialize, Debug)]
struct ToolCallResult {
    content: Vec<McpContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightKind {
    QueryTerm,
    FstEntity,
    Both,
}

#[derive(Debug, Clone)]
struct HighlightSpan {
    start: usize,
    end: usize,
    kind: HighlightKind,
    label: String,
}

// Caching Index and Spelling Corrector
struct CachedData {
    path: String,
    mtime: Option<SystemTime>,
    index: std::sync::Arc<Bm25Index>,
    spell: std::sync::Arc<SpellIndex>,
}

static CACHE: Mutex<Option<CachedData>> = Mutex::new(None);

fn get_mtime(path: &Path) -> Option<SystemTime> {
    if path.is_file() {
        fs::metadata(path).and_then(|m| m.modified()).ok()
    } else if path.is_dir() {
        let mut max_time = fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut files = Vec::new();
        let _ = collect_files(path, &mut files);
        for p in files {
            if let Ok(m) = fs::metadata(p).and_then(|meta| meta.modified()) {
                if let Some(ref current_max) = max_time {
                    if m > *current_max {
                        max_time = Some(m);
                    }
                } else {
                    max_time = Some(m);
                }
            }
        }
        max_time
    } else {
        None
    }
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            if path.is_dir() {
                collect_files(&path, files)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if ext_lower == "md" || ext_lower == "markdown" || ext_lower == "txt" {
                        files.push(path);
                    }
                }
            }
        }
    }
    Ok(())
}

fn build_sections_from_path(path: &Path) -> Result<Vec<Section>, String> {
    let path_str = path.to_string_lossy().to_string();
    let is_directory = path.is_dir();
    let is_csv = !is_directory && path_str.to_ascii_lowercase().ends_with(".csv");

    if is_directory {
        let mut files = Vec::new();
        if let Err(e) = collect_files(path, &mut files) {
            return Err(format!("Failed to read directory {}: {}", path_str, e));
        }
        files.sort();

        let mut dir_sections = Vec::new();
        for file_path in files {
            let filename = file_path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let content = match fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Warning: Failed to read file {:?}: {}", file_path, e);
                    continue;
                }
            };

            let ext = file_path.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();

            if ext == "md" || ext == "markdown" {
                let parsed = parse_markdown(&content);
                for mut sec in parsed {
                    sec.filename = Some(filename.clone());
                    dir_sections.push(sec);
                }
            } else if ext == "txt" {
                dir_sections.push(Section {
                    title: filename.clone(),
                    body: content,
                    line_number: 1,
                    filename: Some(filename),
                });
            }
        }
        Ok(dir_sections)
    } else if is_csv {
        let md_content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read CSV document {}: {}", path_str, e))?;

        let mut csv_sections = Vec::new();
        let mut lines = md_content.lines();
        if let Some(header_line) = lines.next() {
            let headers = crate::parse_csv_line(header_line);

            let title_col_idx = headers.iter()
                .position(|h| {
                    let h_lower = h.trim().to_lowercase();
                    h_lower == "name" || h_lower == "phrase" || h_lower == "title" || h_lower == "id"
                })
                .unwrap_or(0);

            for (i, line) in lines.enumerate() {
                let line_num = i + 2;
                if line.trim().is_empty() {
                    continue;
                }
                let cells = crate::parse_csv_line(line);
                if cells.is_empty() || (cells.len() == 1 && cells[0].trim().is_empty()) {
                    continue;
                }

                let title = cells.get(title_col_idx).map(|s| s.trim()).unwrap_or("").to_string();
                let title = if title.is_empty() {
                    format!("Row {}", line_num)
                } else {
                    title
                };

                let mut body_parts = Vec::new();
                for (col_idx, value) in cells.iter().enumerate() {
                    let col_name = headers.get(col_idx).map(|s| s.trim().as_ref()).unwrap_or("column");
                    body_parts.push(format!("{}: {}", col_name, value.trim()));
                }
                let body = body_parts.join(" | ");

                csv_sections.push(Section {
                    title,
                    body,
                    line_number: line_num,
                    filename: None,
                });
            }
        }
        Ok(csv_sections)
    } else {
        let md_content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read document {}: {}", path_str, e))?;

        let ext = path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        if ext == "md" || ext == "markdown" {
            Ok(parse_markdown(&md_content))
        } else {
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("document")
                .to_string();
            Ok(vec![Section {
                title: filename.clone(),
                body: md_content,
                line_number: 1,
                filename: Some(filename),
            }])
        }
    }
}

fn get_or_build_index(
    target_path: &str,
    tagger: Option<&Tagger>,
) -> Result<(std::sync::Arc<Bm25Index>, std::sync::Arc<SpellIndex>), String> {
    let path = Path::new(target_path);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", target_path));
    }

    let mtime = get_mtime(path);

    {
        let guard = CACHE.lock().unwrap();
        if let Some(ref cached) = *guard {
            if cached.path == target_path && cached.mtime == mtime {
                return Ok((cached.index.clone(), cached.spell.clone()));
            }
        }
    }

    let sections = build_sections_from_path(path)?;
    let index = Bm25Index::build(sections, tagger);

    let fst_phrases = tagger.map(|t| t.phrases().to_vec()).unwrap_or_default();
    let corpus_terms: Vec<Vec<u8>> = index.posting_lists.keys().cloned().collect();
    let spell = SpellIndex::build(&fst_phrases, &corpus_terms);

    let index_arc = std::sync::Arc::new(index);
    let spell_arc = std::sync::Arc::new(spell);

    {
        let mut guard = CACHE.lock().unwrap();
        *guard = Some(CachedData {
            path: target_path.to_string(),
            mtime,
            index: index_arc.clone(),
            spell: spell_arc.clone(),
        });
    }

    Ok((index_arc, spell_arc))
}

fn merge_spans(mut spans: Vec<HighlightSpan>) -> Vec<HighlightSpan> {
    if spans.is_empty() {
        return Vec::new();
    }
    spans.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| b.end.cmp(&a.end)));

    let mut merged = Vec::new();
    let mut cur = spans[0].clone();

    for next in spans.into_iter().skip(1) {
        if next.start < cur.end {
            cur.end = cur.end.max(next.end);
            cur.kind = match (cur.kind, next.kind) {
                (HighlightKind::Both, _) | (_, HighlightKind::Both) => HighlightKind::Both,
                (HighlightKind::QueryTerm, HighlightKind::FstEntity) => HighlightKind::Both,
                (HighlightKind::FstEntity, HighlightKind::QueryTerm) => HighlightKind::Both,
                (k, _) => k,
            };
            if !next.label.is_empty() {
                if cur.label.is_empty() {
                    cur.label = next.label;
                } else if !cur.label.contains(&next.label) {
                    cur.label = format!("{}, {}", cur.label, next.label);
                }
            }
        } else {
            merged.push(cur);
            cur = next;
        }
    }
    merged.push(cur);
    merged
}

fn get_snippet_and_spans(
    text: &str,
    spans: &[HighlightSpan],
) -> (String, Vec<HighlightSpan>) {
    if text.len() <= 400 || spans.is_empty() {
        return (text.to_string(), spans.to_vec());
    }

    let first_span = &spans[0];
    let start_char = first_span.start;

    let mut window_start = if start_char > 100 { start_char - 100 } else { 0 };

    while window_start > 0 && !text.is_char_boundary(window_start) {
        window_start -= 1;
    }

    while window_start > 0 {
        let mut prev = window_start - 1;
        while prev > 0 && !text.is_char_boundary(prev) {
            prev -= 1;
        }
        let ch = text[prev..window_start].chars().next().unwrap();
        if ch.is_whitespace() {
            break;
        }
        window_start = prev;
    }

    let mut window_end = (window_start + 400).min(text.len());
    while window_end < text.len() && !text.is_char_boundary(window_end) {
        window_end += 1;
    }

    let snippet = text[window_start..window_end].to_string();

    let mut shifted_spans = Vec::new();
    for span in spans {
        if span.start >= window_start && span.end <= window_end {
            shifted_spans.push(HighlightSpan {
                start: span.start - window_start,
                end: span.end - window_start,
                kind: span.kind,
                label: span.label.clone(),
            });
        }
    }

    let mut prefix = String::new();
    if window_start > 0 {
        prefix = String::from("... ");
        for span in &mut shifted_spans {
            span.start += 4;
            span.end += 4;
        }
    }
    let suffix = if window_end < text.len() { " ..." } else { "" };

    (format!("{}{}{}", prefix, snippet, suffix), shifted_spans)
}

fn format_markdown_highlighted_text(text: &str, spans: &[HighlightSpan]) -> String {
    let mut out = String::new();
    let mut last_idx = 0;
    for span in spans {
        if span.start > last_idx {
            out.push_str(&text[last_idx..span.start]);
        }
        let slice = &text[span.start..span.end];
        match span.kind {
            HighlightKind::QueryTerm => {
                out.push_str(&format!("**{}**", slice));
            }
            HighlightKind::FstEntity => {
                out.push_str(&format!("*{}* (_{}_)", slice, span.label));
            }
            HighlightKind::Both => {
                out.push_str(&format!("***{}*** (_{}_)", slice, span.label));
            }
        }
        last_idx = span.end;
    }
    if last_idx < text.len() {
        out.push_str(&text[last_idx..]);
    }
    out
}

fn get_tools_list() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "lume_tag",
                "description": "Tag an input text block using the FST-backed dictionary (loaded from DATA folder). Exposes high-performance offline entity extraction.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The text block to tag."
                        }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "lume_search",
                "description": "Index a file or directory on-the-fly and perform field-aware BM25 hybrid lexical/semantic search with entity highlights.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_path": {
                            "type": "string",
                            "description": "Absolute or relative path to a Markdown file, text file, CSV file, or directory to index on-the-fly."
                        },
                        "query": {
                            "type": "string",
                            "description": "The hybrid query terms to search for."
                        }
                    },
                    "required": ["target_path", "query"]
                }
            },
            {
                "name": "lume_generate",
                "description": "Builds a local trigram Markov model over a document on-the-fly and synthesizes steered stochastically guided text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_path": {
                            "type": "string",
                            "description": "Absolute or relative path to a Markdown file, text file, CSV file, or directory to train the model on-the-fly."
                        },
                        "seed": {
                            "type": "string",
                            "description": "Optional seed word to start text generation."
                        },
                        "max_tokens": {
                            "type": "integer",
                            "description": "Optional maximum number of tokens to generate (default 150)."
                        }
                    },
                    "required": ["target_path"]
                }
            }
        ]
    })
}

fn execute_tag(tagger: Option<&Tagger>, text: &str) -> Result<serde_json::Value, String> {
    let t = match tagger {
        Some(val) => val,
        None => return Err("FST tagger is not loaded because the DATA environment variable was not set at startup.".to_string()),
    };

    let tags = t.tag(text);

    let mut serialized_tags = Vec::new();
    for tag in tags {
        serialized_tags.push(serde_json::json!({
            "start": tag.start,
            "end": tag.end,
            "surface": tag.surface,
            "id": tag.id,
            "kind": tag.kind,
            "output": tag.output
        }));
    }

    Ok(serde_json::Value::Array(serialized_tags))
}

fn execute_search(
    tagger: Option<&Tagger>,
    target_path: &str,
    query: &str,
) -> Result<serde_json::Value, String> {
    let (index, spell_index) = get_or_build_index(target_path, tagger)?;

    let variant = match env::var("VARIANT").as_deref() {
        Ok("plus") => SearchVariant::Plus,
        Ok("l") => SearchVariant::L,
        _ => SearchVariant::Classic,
    };

    let params = Bm25Params {
        k1: env::var("K1").ok().and_then(|s| s.parse().ok()).unwrap_or(1.2),
        b: env::var("B").ok().and_then(|s| s.parse().ok()).unwrap_or(0.75),
        delta: env::var("DELTA").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0),
        title_weight: env::var("TITLE_WEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0),
        body_weight: env::var("BODY_WEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0),
    };

    let hits = index.search(query, variant, &params, tagger);

    let mut markdown = String::new();

    let query_tokens = tokenize(query);
    let mut corrected_terms = Vec::new();
    let mut corrected_any = false;
    for q_tok in &query_tokens {
        if let Ok(q_word) = String::from_utf8(q_tok.bytes.clone()) {
            let q_word_lower = q_word.to_lowercase();
            if q_word_lower.chars().any(|c| c.is_alphabetic()) && !spell_index.vocab_set.contains(&q_word_lower) {
                let suggestions = spell_index.correct_word(&q_word_lower, 1);
                if !suggestions.is_empty() {
                    corrected_terms.push(suggestions[0].0.clone());
                    corrected_any = true;
                } else {
                    corrected_terms.push(q_word);
                }
            } else {
                corrected_terms.push(q_word);
            }
        }
    }
    if corrected_any {
        let corrected_query = corrected_terms.join(" ");
        markdown.push_str(&format!("💡 *Did you mean:* **{}** ? (corrected from *{}*)\n\n", corrected_query, query));
    }

    if let Some(t) = tagger {
        let query_tags = t.tag(query);
        if !query_tags.is_empty() {
            markdown.push_str("🔍 *Matched Query Entities:* ");
            let mut tags_formatted = Vec::new();
            for tag in query_tags {
                tags_formatted.push(format!("**{}** [_{}_ ➔ `{}`]", tag.surface, tag.kind, tag.output));
            }
            markdown.push_str(&tags_formatted.join(", "));
            markdown.push_str("\n\n");
        }
    }

    markdown.push_str(&format!("Found **{}** ranked results:\n\n", hits.len()));

    for (rank, hit) in hits.iter().enumerate() {
        let section = &index.sections[hit.section_index];
        let title_to_show = if let Some(ref filename) = section.filename {
            format!("{} ➔ {}", filename, section.title)
        } else {
            section.title.clone()
        };

        markdown.push_str(&format!("### Rank {} | Score: {:.4}\n", rank + 1, hit.score));
        markdown.push_str(&format!("* **Header:** {} (Line {})\n", title_to_show, section.line_number));

        let mut spans = Vec::new();
        let query_tokens = tokenize(query);
        let body_tokens = tokenize(&section.body);
        for b_tok in body_tokens {
            if query_tokens.iter().any(|q| q.bytes == b_tok.bytes) {
                spans.push(HighlightSpan {
                    start: b_tok.start,
                    end: b_tok.end,
                    kind: HighlightKind::QueryTerm,
                    label: String::new(),
                });
            }
        }

        if let Some(t) = tagger {
            let doc_tags = t.tag(&section.body);
            for tag in doc_tags {
                spans.push(HighlightSpan {
                    start: tag.start,
                    end: tag.end,
                    kind: HighlightKind::FstEntity,
                    label: format!("{} ({})", tag.output, tag.kind),
                });
            }
        }

        let merged_spans = merge_spans(spans);
        let (snippet, shifted_spans) = get_snippet_and_spans(&section.body, &merged_spans);

        let highlighted_snippet = format_markdown_highlighted_text(&snippet, &shifted_spans);
        markdown.push_str(&format!("> {}\n\n", highlighted_snippet.trim()));
    }

    Ok(serde_json::Value::String(markdown))
}

fn execute_generate(
    tagger: Option<&Tagger>,
    target_path: &str,
    seed: Option<&str>,
    max_tokens: Option<usize>,
) -> Result<serde_json::Value, String> {
    let (index, _) = get_or_build_index(target_path, tagger)?;

    let bodies: Vec<&str> = index.sections.iter().map(|s| s.body.as_str()).collect();
    if bodies.is_empty() {
        return Err("No indexed document sections found to train the model.".to_string());
    }

    let chain = MarkovChain::build(&bodies);
    let tokens_to_gen = max_tokens.unwrap_or(150);

    let (text, attention_history) = chain.generate_steered(
        seed,
        tokens_to_gen,
        tagger,
        &index.entity_posting_lists,
    );

    let mut markdown = String::new();
    markdown.push_str("## 🧠 Steered Generated Text\n\n");
    markdown.push_str(&format!("\"{}\"\n\n", text));

    if !attention_history.is_empty() {
        markdown.push_str("### 👁️ FST Attention Feedback Traces\n");
        markdown.push_str("Below is the record of entity activations tracked stochastically in local attention registers:\n\n");
        let mut last_printed_token = 0;
        for (token_idx, register) in &attention_history {
            if token_idx - last_printed_token >= 8 {
                let mut trace_strs = Vec::new();
                for (tag, weight) in register {
                    trace_strs.push(format!("`{}` ({:.2})", tag, weight));
                }
                markdown.push_str(&format!("- **Token #{:3}**: {}\n", token_idx, trace_strs.join(", ")));
                last_printed_token = *token_idx;
            }
        }
    }

    Ok(serde_json::Value::String(markdown))
}

pub fn run(_args: Vec<String>) {
    // Standard error is used for logging/debugging so stdout is completely reserved for JSON-RPC.
    let tagger = match Tagger::from_env() {
        Ok(Some(t)) => {
            eprintln!(
                "Loaded FST dictionary: {} records ({} keys) from DATA (kinds: {})",
                t.record_count(),
                t.len(),
                t.kinds().join(", ")
            );
            Some(t)
        }
        Ok(None) => {
            eprintln!("No DATA environment variable set. FST tagging disabled.");
            None
        }
        Err(e) => {
            eprintln!("Failed to load DATA directory: {}", e);
            None
        }
    };

    eprintln!("Lume MCP Server started successfully on stdio.");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Error reading standard input: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to parse JSON-RPC request: {}. Line: {}", e, line);
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                    }),
                    id: serde_json::Value::Null,
                };
                if let Ok(resp_str) = serde_json::to_string(&err_resp) {
                    let _ = writeln!(stdout, "{}", resp_str);
                    let _ = stdout.flush();
                }
                continue;
            }
        };

        let response = match request.method.as_str() {
            "initialize" => {
                let init_result = serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "lume-mcp-server",
                        "version": "0.1.0"
                    }
                });
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(init_result),
                    error: None,
                    id: request.id.clone(),
                }
            }
            "notifications/initialized" | "initialized" => {
                // Initialized notification. No response expected/required.
                continue;
            }
            "tools/list" => {
                let list_result = get_tools_list();
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(list_result),
                    error: None,
                    id: request.id.clone(),
                }
            }
            "tools/call" => {
                let params: Result<ToolCallParams, _> = serde_json::from_value(request.params.clone());
                match params {
                    Ok(p) => {
                        let tool_result = match p.name.as_str() {
                            "lume_tag" => {
                                let text = p.arguments.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                match execute_tag(tagger.as_ref(), text) {
                                    Ok(json_val) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: serde_json::to_string_pretty(&json_val).unwrap_or_default(),
                                        }],
                                        is_error: false,
                                    },
                                    Err(err_msg) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: format!("Error: {}", err_msg),
                                        }],
                                        is_error: true,
                                    },
                                }
                            }
                            "lume_search" => {
                                let target_path = p.arguments.get("target_path").and_then(|v| v.as_str()).unwrap_or("");
                                let query = p.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("");
                                match execute_search(tagger.as_ref(), target_path, query) {
                                    Ok(json_val) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: json_val.as_str().unwrap_or("").to_string(),
                                        }],
                                        is_error: false,
                                    },
                                    Err(err_msg) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: format!("Error: {}", err_msg),
                                        }],
                                        is_error: true,
                                    },
                                }
                            }
                            "lume_generate" => {
                                let target_path = p.arguments.get("target_path").and_then(|v| v.as_str()).unwrap_or("");
                                let seed = p.arguments.get("seed").and_then(|v| v.as_str());
                                let max_tokens = p.arguments.get("max_tokens").and_then(|v| v.as_u64()).map(|n| n as usize);
                                match execute_generate(tagger.as_ref(), target_path, seed, max_tokens) {
                                    Ok(json_val) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: json_val.as_str().unwrap_or("").to_string(),
                                        }],
                                        is_error: false,
                                    },
                                    Err(err_msg) => ToolCallResult {
                                        content: vec![McpContent {
                                            content_type: "text".to_string(),
                                            text: format!("Error: {}", err_msg),
                                        }],
                                        is_error: true,
                                    },
                                }
                            }
                            unknown => ToolCallResult {
                                content: vec![McpContent {
                                    content_type: "text".to_string(),
                                    text: format!("Error: Unknown tool {}", unknown),
                                }],
                                is_error: true,
                            },
                        };

                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: Some(serde_json::to_value(&tool_result).unwrap_or(serde_json::Value::Null)),
                            error: None,
                            id: request.id.clone(),
                        }
                    }
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32602,
                            message: format!("Invalid params: {}", e),
                        }),
                        id: request.id.clone(),
                    },
                }
            }
            other => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", other),
                }),
                id: request.id.clone(),
            },
        };

        if let Ok(resp_str) = serde_json::to_string(&response) {
            let _ = writeln!(stdout, "{}", resp_str);
            let _ = stdout.flush();
        }
    }
}
