//! Hatcher Semantic Boosting Hybrid Search Example
//!
//! Demonstrates Erik Hatcher's two-shot "Semantic Boosting" pattern blending:
//!   - Stage 1: Remote Dense Vector Semantic Retrieval (via `https://shivvr.nuts.services/`)
//!   - Stage 2: Local High-Performance Lexical BM25 Ranking
//!
//! Boosting Formulation:
//!   Score_hybrid = Score_BM25 * (1.0 + alpha * Score_semantic)
//!
//! Usage:
//!   cargo run --bin hatcher-boost [target.md] [optional search term]
//!
//! If no search term is provided, the program enters a beautiful interactive REPL.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;

use lume::bm25::{parse_markdown, Bm25Index, Bm25Params, SearchVariant, Section};
use lume::{tokenize, Tagger};

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct IngestPayload<'a> {
    text: &'a str,
    source: &'a str,
}

#[derive(Deserialize, Debug, Clone)]
struct SearchResult {
    chunk_id: String,
    score: f64,
    text: String,
    source: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SearchResponse {
    query: String,
    results: Vec<SearchResult>,
    time_ms: usize,
}

/// Simple percent encoder to avoid adding external dependencies.
fn percent_encode(s: &str) -> String {
    let mut encoded = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => {
                encoded.push('+');
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

fn collect_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
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

fn main() {
    // Setup panic hook to handle broken pipe gracefully (like in search.rs)
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

    println!("\x1B[1;36m========================================================================\x1B[0m");
    println!("\x1B[1;35m🚀  LUME HYBRID SEARCH: ERIK HATCHER'S SEMANTIC BOOSTING ENGINE  🚀\x1B[0m");
    println!("\x1B[1;36m========================================================================\x1B[0m");

    let mut args: Vec<String> = env::args().skip(1).collect();

    let target_file = if !args.is_empty() {
        args.remove(0)
    } else {
        let default_file = "examples/tutorial.md".to_string();
        if !std::path::Path::new(&default_file).exists() {
            eprintln!("\x1B[1;31mError: No target markdown file provided, and default 'examples/tutorial.md' does not exist.\x1B[0m");
            eprintln!("Usage: cargo run --bin hatcher-boost <target.md> [optional query]");
            process::exit(1);
        }
        println!("\x1B[33mNo target file specified. Defaulting to: {}\x1B[0m", default_file);
        default_file
    };

    let query_arg = if !args.is_empty() {
        Some(args.join(" "))
    } else {
        None
    };

    // Load FST Tagger if present
    let tagger = match Tagger::from_env() {
        Ok(Some(t)) => {
            println!(
                "\x1B[32mLoaded FST dictionary: {} records (kinds: {})\x1B[0m",
                t.record_count(),
                t.kinds().join(", ")
            );
            Some(t)
        }
        _ => {
            println!("\x1B[33mNo DATA environment variable set. FST tagging disabled.\x1B[0m");
            None
        }
    };

    // Read and parse sections
    let path = std::path::Path::new(&target_file);
    let mut sections = Vec::new();

    if path.is_dir() {
        let mut files = Vec::new();
        if let Err(e) = collect_files(path, &mut files) {
            eprintln!("\x1B[1;31mFailed to read directory {}:\x1B[0m {}", target_file, e);
            process::exit(1);
        }
        files.sort();
        for f in files {
            let filename = f.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if let Ok(content) = fs::read_to_string(&f) {
                if f.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()) == Some("txt".to_string()) {
                    sections.push(Section {
                        title: filename,
                        body: content,
                        line_number: 1,
                    });
                } else {
                    let parsed = parse_markdown(&content);
                    for mut sec in parsed {
                        sec.title = format!("{} ➔ {}", filename, sec.title);
                        sections.push(sec);
                    }
                }
            }
        }
    } else {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("\x1B[1;31mFailed to read target file {}:\x1B[0m {}", target_file, e);
                process::exit(1);
            }
        };
        if target_file.ends_with(".txt") {
            sections.push(Section {
                title: target_file.clone(),
                body: content,
                line_number: 1,
            });
        } else {
            sections = parse_markdown(&content);
        }
    }

    if sections.is_empty() {
        eprintln!("\x1B[1;31mError: No valid search sections found in corpus.\x1B[0m");
        process::exit(1);
    }

    println!("\x1B[32mLoaded {} sections for search corpus.\x1B[0m", sections.len());

    // Build Local BM25 Index
    println!("\x1B[34mBuilding local BM25 index...\x1B[0m");
    let bm25_index = Bm25Index::build(sections.clone(), tagger.as_ref());
    println!("\x1B[32mBM25 Index compiled successfully.\x1B[0m");

    // Initialize shivvr session
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let session_id = format!("lume-hatcher-{}", timestamp);

    println!("\x1B[34mInitializing remote vector store session: {}...\x1B[0m", session_id);

    // Ingest all sections into the remote vector store
    for (idx, sec) in sections.iter().enumerate() {
        let text = format!("Header: {}\nContent: {}", sec.title, sec.body);
        let source_str = idx.to_string();
        
        let url = format!("https://shivvr.nuts.services/temp/{}/ingest", session_id);
        
        print!("  ➔ Ingesting chunk [{}/{}] \"{}\"... ", idx + 1, sections.len(), sec.title);
        io::stdout().flush().unwrap();

        let payload = IngestPayload {
            text: &text,
            source: &source_str,
        };

        match ureq::post(&url)
            .send_json(&payload)
        {
            Ok(res) => {
                if res.status() == 200 || res.status() == 201 {
                    println!("\x1B[32mOK (Status {})\x1B[0m", res.status());
                } else {
                    println!("\x1B[31mFailed (Status {})\x1B[0m", res.status());
                }
            }
            Err(e) => {
                println!("\x1B[1;31mError: {}\x1B[0m", e);
                println!("\x1B[33mWarning: Semantic store ingestion failed. Clean up and exit.\x1B[0m");
                cleanup_session(&session_id);
                process::exit(1);
            }
        }
    }
    println!("\x1B[32mSuccessfully ingested entire corpus into shivvr.nuts.services.\x1B[0m\n");

    let params = Bm25Params::default();
    let variant = SearchVariant::Classic;
    let alpha: f64 = env::var("ALPHA")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0);

    println!("\x1B[1;33mHybrid configuration:\x1B[0m");
    println!("  BM25 Variant: \x1B[35m{:?}\x1B[0m", variant);
    println!("  Semantic Boost Factor (ALPHA): \x1B[35m{:.2}\x1B[0m", alpha);
    println!("  Equation: \x1B[1;32mScore_hybrid = Score_BM25 * (1.0 + {} * Similarity_semantic)\x1B[0m", alpha);
    println!();

    if let Some(query) = query_arg {
        execute_hybrid_search(&bm25_index, tagger.as_ref(), &session_id, &query, variant, &params, alpha);
        cleanup_session(&session_id);
    } else {
        // Run REPL
        let mut stdout = io::stdout();
        println!("\x1B[1;32mInteractive REPL Mode. Type 'exit' or 'quit' to clean up and exit.\x1B[0m\n");

        loop {
            print!("\x1B[1;32mhybrid-search > \x1B[0m");
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

            execute_hybrid_search(&bm25_index, tagger.as_ref(), &session_id, query, variant, &params, alpha);
            println!();
        }

        cleanup_session(&session_id);
    }

    println!("\x1B[1;32mHybrid Search Session closed. Thank you!\x1B[0m");
}

fn cleanup_session(session_id: &str) {
    println!("\n\x1B[34mCleaning up ephemeral remote session {}...\x1B[0m", session_id);
    let url = format!("https://shivvr.nuts.services/temp/{}", session_id);
    match ureq::delete(&url).call() {
        Ok(res) => {
            println!("\x1B[32mSuccessfully deleted remote temporary session (Status {}).\x1B[0m", res.status());
        }
        Err(e) => {
            println!("\x1B[33mWarning: Failed to delete session: {} (it will automatically expire in 2 hours).\x1B[0m", e);
        }
    }
}

fn execute_hybrid_search(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    session_id: &str,
    query: &str,
    variant: SearchVariant,
    params: &Bm25Params,
    alpha: f64,
) {
    println!("\x1B[1;34m========================================================================\x1B[0m");
    println!("\x1B[1;34m🔍  QUERY: \"{}\"\x1B[0m", query);
    println!("\x1B[1;34m========================================================================\x1B[0m");

    // --- STAGE 1: SEMANTIC VECTOR RETRIEVAL (REMOTE) ---
    let sem_start = Instant::now();
    let encoded_query = percent_encode(query);
    let url = format!("https://shivvr.nuts.services/temp/{}/search?q={}&n=15", session_id, encoded_query);

    let semantic_results = match ureq::get(&url).call() {
        Ok(res) => {
            match res.into_json::<SearchResponse>() {
                Ok(resp) => resp.results,
                Err(e) => {
                    eprintln!("\x1B[31mError parsing semantic search JSON: {}\x1B[0m", e);
                    Vec::new()
                }
            }
        }
        Err(e) => {
            eprintln!("\x1B[31mError querying semantic search service: {}\x1B[0m", e);
            Vec::new()
        }
    };
    let sem_elapsed = sem_start.elapsed();

    // Map semantic results: section_index -> semantic_score
    let mut semantic_map: HashMap<usize, (usize, f64)> = HashMap::new();
    for (rank, res) in semantic_results.iter().enumerate() {
        if let Some(ref src) = res.source {
            if let Ok(idx) = src.parse::<usize>() {
                semantic_map.insert(idx, (rank, res.score));
            }
        }
    }

    // --- STAGE 2: LEXICAL RETRIEVAL (LOCAL BM25) ---
    let lex_start = Instant::now();
    let bm25_hits = index.search(query, variant, params, tagger);
    let lex_elapsed = lex_start.elapsed();

    // Map BM25 results: section_index -> (rank, score)
    let mut bm25_map: HashMap<usize, (usize, f64)> = HashMap::new();
    for (rank, hit) in bm25_hits.iter().enumerate() {
        bm25_map.insert(hit.section_index, (rank, hit.score));
    }

    // --- STAGE 3: HATCHER SEMANTIC BOOST BLENDING ---
    let blend_start = Instant::now();
    
    // We combine the candidate pools. Erik Hatcher's Semantic Boosting acts as a boost to BM25 scores.
    // If a document is matched by BM25, we boost it. If it is ONLY matched by semantic search,
    // we can either add it with a low base score or leave it out depending on whether we want a lexical-first or pure-union recall.
    // Erik Hatcher's "Semantic Boosting" specifically targets using vector similarity scores to boost the matches of a lexical query,
    // allowing structural constraints (like filters) and precision of full-text queries to remain dominant while ranking gets the conceptual boost.
    // Thus, the candidate pool is based on the BM25 hits, and we apply boosts to those hits.
    let mut hybrid_hits: Vec<(usize, f64, f64, f64, bool)> = Vec::new();

    for hit in &bm25_hits {
        let idx = hit.section_index;
        let bm25_score = hit.score;
        
        let (sem_score, boosted) = if let Some((_, sem_s)) = semantic_map.get(&idx) {
            (*sem_s, true)
        } else {
            (0.0, false)
        };

        // Score formulation: Score_hybrid = Score_BM25 * (1.0 + alpha * Score_semantic)
        let hybrid_score = bm25_score * (1.0 + alpha * sem_score);

        hybrid_hits.push((idx, bm25_score, sem_score, hybrid_score, boosted));
    }

    // Sort hybrid hits descending by hybrid score
    hybrid_hits.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    let blend_elapsed = blend_start.elapsed();

    // --- PRINT DETAILED COMPARATIVE VIEW ---
    println!("\x1B[1;32mTIMINGS:\x1B[0m");
    println!("  Remote Semantic Search (ONNX):  \x1B[36m{:.2?}\x1B[0m (returned {} docs)", sem_elapsed, semantic_results.len());
    println!("  Local Lexical BM25 Search:      \x1B[36m{:.2?}\x1B[0m (returned {} docs)", lex_elapsed, bm25_hits.len());
    println!("  Hatcher Semantic Boosting:     \x1B[36m{:.2?}\x1B[0m", blend_elapsed);
    println!();

    // 1. Lexical BM25 Matches Column
    println!("\x1B[1;4m1. PURE LEXICAL BM25 TOP MATCHES:\x1B[0m");
    if bm25_hits.is_empty() {
        println!("  (No matches)");
    } else {
        for (r, hit) in bm25_hits.iter().take(5).enumerate() {
            let sec = &index.sections[hit.section_index];
            println!("  [{}] Score: \x1B[35m{:.4}\x1B[0m | \x1B[1m{}\x1B[0m", r + 1, hit.score, sec.title);
        }
    }
    println!();

    // 2. Semantic Matches Column
    println!("\x1B[1;4m2. PURE SEMANTIC (ONNX) TOP MATCHES:\x1B[0m");
    if semantic_results.is_empty() {
        println!("  (No matches)");
    } else {
        for (r, res) in semantic_results.iter().take(5).enumerate() {
            println!("  [{}] Sim: \x1B[35m{:.4}\x1B[0m | \x1B[1m{}\x1B[0m", r + 1, res.score, res.text.lines().next().unwrap_or(""));
        }
    }
    println!();

    // 3. Hybrid Matches Column
    println!("\x1B[1;4;33m3. ERIK HATCHER SEMANTIC BOOSTED HYBRID TOP MATCHES:\x1B[0m");
    if hybrid_hits.is_empty() {
        println!("  (No matches)");
    } else {
        for (r, &(idx, bm25_s, sem_s, hybrid_s, boosted)) in hybrid_hits.iter().take(5).enumerate() {
            let sec = &index.sections[idx];
            let boost_indicator = if boosted {
                format!("\x1B[32m✨ Boosted (+{:.1}% from semantic Sim {:.4})\x1B[0m", (sem_s * alpha * 100.0), sem_s)
            } else {
                "\x1B[31m✖ No Semantic Match (unboosted)\x1B[0m".to_string()
            };
            
            println!("  [{}] Hybrid Score: \x1B[1;33m{:.4}\x1B[0m (BM25: {:.4}) | \x1B[1m{}\x1B[0m", r + 1, hybrid_s, bm25_s, sec.title);
            println!("      └─ {}", boost_indicator);
        }
    }
    println!("\x1B[1;34m========================================================================\x1B[0m\n");
}
