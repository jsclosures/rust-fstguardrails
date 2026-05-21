use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::time::Instant;

use text_tagger::bm25::{parse_markdown, Bm25Index, Bm25Params, SearchVariant};
use text_tagger::{tokenize, Tagger};

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

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();

    // Determine scoring params from environment variables
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

    // Load FST tagger
    let tagger = match Tagger::from_env() {
        Ok(Some(t)) => {
            eprintln!(
                "\x1B[32mLoaded FST dictionary: {} records ({} keys) from DATA (kinds: {})\x1B[0m",
                t.record_count(),
                t.len(),
                t.kinds().join(", ")
            );
            Some(t)
        }
        _ => {
            eprintln!("\x1B[33mNo DATA environment variable set or loaded. FST tagging disabled.\x1B[0m");
            None
        }
    };

    if args.is_empty() {
        eprintln!("\x1B[1;31mUsage:\x1B[0m search <target.md> [optional search terms...]");
        process::exit(2);
    }

    let md_path = args.remove(0);
    let md_content = match fs::read_to_string(&md_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\x1B[1;31mFailed to read Markdown document {md_path}:\x1B[0m {e}");
            process::exit(1);
        }
    };

    // Index Markdown
    let start_indexing = Instant::now();
    let sections = parse_markdown(&md_content);
    let index = Bm25Index::build(sections);
    let index_time = start_indexing.elapsed();
    eprintln!(
        "\x1B[32mIndexed {} Markdown sections in {:.2?}\x1B[0m",
        index.num_docs, index_time
    );

    // If query terms are passed as args, execute one-shot search
    if !args.is_empty() {
        let query = args.join(" ");
        execute_search(&index, tagger.as_ref(), &query, variant, &params);
    } else {
        // Run Interactive REPL loop
        run_repl(&index, tagger.as_ref(), variant, &params);
    }
}

fn execute_search(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    query: &str,
    variant: SearchVariant,
    params: &Bm25Params,
) {
    println!("\x1B[1;34mSearching for:\x1B[0m \"{}\" (Variant: {:?})", query, variant);
    
    // Tag query itself with FST if enabled
    if let Some(ref t) = tagger {
        let query_tags = t.tag(query);
        if !query_tags.is_empty() {
            print!("  \x1B[32m└─ Matched Query Entities:\x1B[0m ");
            for (idx, tag) in query_tags.iter().enumerate() {
                if idx > 0 {
                    print!(", ");
                }
                print!("\x1B[1m{}\x1B[0m [id={}, type={}]", tag.surface, tag.id, tag.kind);
            }
            println!();
        }
    }

    let start_search = Instant::now();
    let hits = index.search(query, variant, params);
    let elapsed = start_search.elapsed();

    println!("\x1B[34mFound {} ranked results in {:.2?}\x1B[0m\n", hits.len(), elapsed);

    for (rank, hit) in hits.iter().enumerate() {
        let section = &index.sections[hit.section_index];
        println!(
            "\x1B[1;35mRank {} | Score: {:.4}\x1B[0m",
            rank + 1, hit.score
        );
        println!(
            "\x1B[1;36mHeader: {} (Line {})\x1B[0m",
            section.title, section.line_number
        );
        
        // 1. Gather all highlight spans
        let mut spans = Vec::new();
        
        // Match 1: Query term matches (case-folded / lowercased)
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
        
        // Match 2: FST entity tags matches
        if let Some(ref t) = tagger {
            let doc_tags = t.tag(&section.body);
            for tag in doc_tags {
                spans.push(HighlightSpan {
                    start: tag.start,
                    end: tag.end,
                    kind: HighlightKind::FstEntity,
                    label: format!("{} ({})", tag.id, tag.kind),
                });
            }
        }
        
        // Merge overlapping highlight spans
        let merged_spans = merge_spans(spans);
        
        // Get Snippet window of 400 chars around the first match
        let (snippet, shifted_spans) = get_snippet_and_spans(&section.body, &merged_spans);
        
        // Print snippet with styled highlights
        print!("  ");
        print_highlighted_text(&snippet, &shifted_spans);
        println!("\x1B[38;5;244m────────────────────────────────────────────────────────────\x1B[0m");
    }
}

fn run_repl(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    variant: SearchVariant,
    params: &Bm25Params,
) {
    println!();
    println!("      \x1B[1;36m▄▀▀▄        Antigravity Search Mesh REPL\x1B[0m");
    println!("     \x1B[36m▀▀▀▀▀▀       Field-Aware BM25 + FST Entity Tagger\x1B[0m");
    println!("    \x1B[1;34m▀▀▀▀▀▀▀▀      Zero External Dependencies (StdLib Only)\x1B[0m");
    println!("────────────────────────────────────────────────────────────");
    println!("Type your search query and press Enter. Type 'exit' or 'quit' to end.");
    println!();

    let mut stdout = io::stdout();

    loop {
        print!("\x1B[1;32msearch > \x1B[0m");
        let _ = stdout.flush();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            break;
        }

        let query = line.trim();
        if query.is_empty() {
            continue;
        }
        if query == "exit" || query == "quit" {
            break;
        }

        execute_search(index, tagger, query, variant, params);
        println!();
    }
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
            // Overlap!
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
    
    // Focus on first span
    let first_span = &spans[0];
    let start_char = first_span.start;
    
    let mut window_start = if start_char > 100 { start_char - 100 } else { 0 };
    // Find nearest space/newline backwards
    while window_start > 0 && &text[window_start..window_start+1] != " " && &text[window_start..window_start+1] != "\n" {
        window_start -= 1;
    }
    if window_start > 0 {
        window_start += 1;
    }
    
    let window_end = (window_start + 400).min(text.len());
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

fn print_highlighted_text(text: &str, spans: &[HighlightSpan]) {
    let mut last_idx = 0;
    for span in spans {
        if span.start > last_idx {
            print!("{}", &text[last_idx..span.start]);
        }
        let slice = &text[span.start..span.end];
        match span.kind {
            HighlightKind::QueryTerm => {
                // Bold Yellow for matching BM25 query terms
                print!("\x1B[1;33m{}\x1B[0m", slice);
            }
            HighlightKind::FstEntity => {
                // Underlined Green for FST Tag matched terms
                print!("\x1B[4;32m{}\x1B[0m \x1B[32m[{}]\x1B[0m", slice, span.label);
            }
            HighlightKind::Both => {
                // Bold Underlined Cyan for term present in both BM25 query and FST tags
                print!("\x1B[1;4;36m{}\x1B[0m \x1B[36m[{}]\x1B[0m", slice, span.label);
            }
        }
        last_idx = span.end;
    }
    if last_idx < text.len() {
        print!("{}", &text[last_idx..]);
    }
    println!();
}
