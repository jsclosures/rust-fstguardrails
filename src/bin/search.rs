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
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            Some(*s)
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            Some(s.as_str())
        } else {
            None
        };
        
        if let Some(m) = msg {
            if m.contains("failed printing to stdout") || m.contains("The pipe is being closed") || m.contains("BrokenPipe") {
                std::process::exit(0);
            }
        }
        
        eprintln!("{}", info);
    }));

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
    let index = Bm25Index::build(sections, tagger.as_ref());
    let index_time = start_indexing.elapsed();
    eprintln!(
        "\x1B[32mIndexed {} Markdown sections in {:.2?}\x1B[0m",
        index.num_docs, index_time
    );

    // If query terms are passed as args, check for specialized commands or execute one-shot search
    if !args.is_empty() {
        let cmd = args[0].trim().to_lowercase();
        if cmd == "graph" {
            let min_similarity = args.get(1).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.02);
            execute_graph(&index, min_similarity);
        } else if cmd == "generate" {
            let seed = args.get(1).map(|s| s.as_str());
            execute_generate(&index, seed);
        } else {
            let query = args.join(" ");
            execute_search(&index, tagger.as_ref(), &query, variant, &params);
        }
    } else {
        // Run Interactive REPL loop
        run_repl(&index, tagger.as_ref(), variant, &params);
    }
}

fn execute_graph(index: &Bm25Index, min_similarity: f64) {
    use text_tagger::semantic_mesh::EntityGraph;
    
    println!("\x1B[1;34mGenerating Semantic Entity Graph (Minimum Similarity: {:.4})...\x1B[0m", min_similarity);
    let start = Instant::now();
    let graph = EntityGraph::build(
        &index.entity_posting_lists,
        &index.entity_kinds,
        &index.entity_labels,
        min_similarity,
    );
    let elapsed = start.elapsed();
    
    // Print ASCII table
    graph.print_ascii_table();
    
    // Serialize to JSON and write to file
    let json_content = graph.to_json();
    let file_path = "monte_cristo_graph.json";
    match fs::write(file_path, json_content) {
        Ok(_) => {
            println!(
                "\x1B[32mSuccessfully wrote relationship mesh ({} nodes, {} edges) to '{}' in {:.2?}\x1B[0m",
                graph.nodes.len(),
                graph.edges.len(),
                file_path,
                elapsed
            );
        }
        Err(e) => {
            eprintln!("\x1B[1;31mFailed to write graph to '{}':\x1B[0m {}", file_path, e);
        }
    }
}

fn execute_generate(index: &Bm25Index, seed: Option<&str>) {
    use text_tagger::semantic_mesh::MarkovChain;
    
    println!("\x1B[1;34mBuilding Trigram Markov Chain Model...\x1B[0m");
    let start_build = Instant::now();
    let bodies: Vec<&str> = index.sections.iter().map(|s| s.body.as_str()).collect();
    let chain = MarkovChain::build(&bodies);
    let build_elapsed = start_build.elapsed();
    println!("\x1B[32mBuilt Markov Chain ({} transition keys) in {:.2?}\x1B[0m", chain.transitions.len(), build_elapsed);
    
    println!("\x1B[1;34mGenerating Dumas-styled passage...\x1B[0m");
    let start_gen = Instant::now();
    let text = chain.generate(seed, 150);
    let gen_elapsed = start_gen.elapsed();
    
    println!();
    println!("\x1B[1;36m\"{}\"\x1B[0m", text);
    println!();
    println!("\x1B[32mGenerated passage in {:.2?}\x1B[0m", gen_elapsed);
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

    // Pairwise Jaccard index between query terms' posting lists
    let query_tokens = tokenize(query);
    if query_tokens.len() > 1 {
        println!("  \x1B[35m└─ Query Term Posting List Jaccard Similarities:\x1B[0m");
        for i in 0..query_tokens.len() {
            for j in i+1..query_tokens.len() {
                let term_a = &query_tokens[i].bytes;
                let term_b = &query_tokens[j].bytes;
                let str_a = String::from_utf8_lossy(term_a);
                let str_b = String::from_utf8_lossy(term_b);
                let list_a = index.posting_lists.get(term_a);
                let list_b = index.posting_lists.get(term_b);
                match (list_a, list_b) {
                    (Some(la), Some(lb)) => {
                        let jaccard = la.jaccard_similarity(lb);
                        println!("     - '{}' vs '{}': {:.4} (Intersection: {}, Union: {})", 
                            str_a, str_b, jaccard, la.intersect(lb).len(), la.union(lb).len());
                    }
                    _ => {
                        println!("     - '{}' vs '{}': 0.0000 (One or both terms not found)", str_a, str_b);
                    }
                }
            }
        }
    }

    let start_search = Instant::now();
    let hits = index.search(query, variant, params, tagger);
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
    println!("Commands:");
    println!("  - Type a query to search the BM25 FST mesh.");
    println!("  - Type \x1B[1mgraph [min_sim]\x1B[0m to compute entity graph & write JSON.");
    println!("  - Type \x1B[1mgenerate [seed]\x1B[0m to generate text styled in Dumas' voice.");
    println!("  - Type \x1B[1mexit\x1B[0m or \x1B[1mquit\x1B[0m to end.");
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

        // Check for special REPL commands
        let parts: Vec<&str> = query.split_whitespace().collect();
        if !parts.is_empty() {
            let cmd = parts[0].to_lowercase();
            if cmd == "graph" {
                let min_similarity = parts.get(1).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.02);
                execute_graph(index, min_similarity);
                println!();
                continue;
            } else if cmd == "generate" {
                let seed = parts.get(1).map(|s| *s);
                execute_generate(index, seed);
                println!();
                continue;
            }
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
    
    // Align window_start to a valid UTF-8 character boundary walking backward
    while window_start > 0 && !text.is_char_boundary(window_start) {
        window_start -= 1;
    }
    
    // Find nearest space/newline backwards
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
