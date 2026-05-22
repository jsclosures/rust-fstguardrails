use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;

use crate::bm25::{parse_markdown, Bm25Index, Bm25Params, SearchVariant, Section};
use crate::Tagger;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct IngestPayload<'a> {
    text: &'a str,
    source: &'a str,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct SearchResult {
    chunk_id: String,
    score: f64,
    text: String,
    source: Option<String>,
}

#[allow(dead_code)]
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

#[derive(Serialize, Deserialize, Debug)]
struct SessionCache {
    corpus_path: String,
    corpus_mtime: u64,
    corpus_size: u64,
    session_id: String,
    created_at: u64,
}

const CACHE_FILE: &str = ".lume-session-cache.json";

fn get_corpus_metadata(path: &std::path::Path) -> io::Result<(u64, u64)> {
    if path.is_file() {
        let meta = fs::metadata(path)?;
        let mtime = meta.modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok((meta.len(), mtime))
    } else if path.is_dir() {
        let mut total_size = 0;
        let mut max_mtime = 0;
        let mut files = Vec::new();
        collect_files(path, &mut files)?;
        for f in files {
            if let Ok(meta) = fs::metadata(f) {
                total_size += meta.len();
                let mtime = meta.modified()
                    .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                    .unwrap_or(0);
                if mtime > max_mtime {
                    max_mtime = mtime;
                }
            }
        }
        Ok((total_size, max_mtime))
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "Invalid path"))
    }
}

fn load_cached_session(corpus_path: &str, current_size: u64, current_mtime: u64) -> Option<String> {
    let cache_path = std::path::Path::new(CACHE_FILE);
    if !cache_path.exists() {
        return None;
    }
    
    let content = fs::read_to_string(cache_path).ok()?;
    let cache: SessionCache = serde_json::from_str(&content).ok()?;
    
    if cache.corpus_path != corpus_path || cache.corpus_size != current_size || cache.corpus_mtime != current_mtime {
        return None;
    }
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
        
    // Ephemeral session expiration limit increased to 7 days (604,800 seconds)
    if now < cache.created_at || now - cache.created_at > 604800 {
        return None;
    }
    
    Some(cache.session_id)
}

fn save_cached_session(corpus_path: &str, size: u64, mtime: u64, session_id: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
        
    let cache = SessionCache {
        corpus_path: corpus_path.to_string(),
        corpus_mtime: mtime,
        corpus_size: size,
        session_id: session_id.to_string(),
        created_at: now,
    };
    
    if let Ok(content) = serde_json::to_string_pretty(&cache) {
        let _ = fs::write(CACHE_FILE, content);
    }
}

fn delete_cached_session() {
    let _ = fs::remove_file(CACHE_FILE);
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SemanticQueryCache {
    corpus_path: String,
    corpus_mtime: u64,
    corpus_size: u64,
    queries: HashMap<String, Vec<SearchResult>>,
}

const SEMANTIC_CACHE_FILE: &str = ".lume-semantic-cache.json";

fn load_semantic_cache(corpus_path: &str, current_size: u64, current_mtime: u64) -> SemanticQueryCache {
    let cache_path = std::path::Path::new(SEMANTIC_CACHE_FILE);
    if cache_path.exists() {
        if let Ok(content) = fs::read_to_string(cache_path) {
            if let Ok(cache) = serde_json::from_str::<SemanticQueryCache>(&content) {
                if cache.corpus_path == corpus_path && cache.corpus_size == current_size && cache.corpus_mtime == current_mtime {
                    return cache;
                }
            }
        }
    }
    SemanticQueryCache {
        corpus_path: corpus_path.to_string(),
        corpus_mtime: current_mtime,
        corpus_size: current_size,
        queries: HashMap::new(),
    }
}

fn save_semantic_cache(cache: &SemanticQueryCache) {
    if let Ok(content) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(SEMANTIC_CACHE_FILE, content);
    }
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

/// Automatically chunks sections whose bodies are too large to avoid 413 Payload Too Large on the neural store
fn chunk_large_sections(sections: Vec<Section>) -> Vec<Section> {
    let mut chunked = Vec::new();
    for sec in sections {
        if sec.body.len() <= 25000 {
            chunked.push(sec);
        } else {
            // Split into paragraphs first
            let paragraphs: Vec<&str> = sec.body.split("\n\n").collect();
            let mut current_chunk = String::new();
            let mut part_num = 1;
            
            for para in paragraphs {
                if current_chunk.len() + para.len() > 25000 {
                    if !current_chunk.is_empty() {
                        chunked.push(Section {
                            title: format!("{} [Part {}]", sec.title, part_num),
                            body: current_chunk.clone(),
                            line_number: sec.line_number,
                            filename: sec.filename.clone(),
                        });
                        current_chunk.clear();
                        part_num += 1;
                    }
                    
                    // If a single paragraph is larger than 25000 characters, split by lines
                    if para.len() > 25000 {
                        let lines: Vec<&str> = para.split('\n').collect();
                        for line in lines {
                            if current_chunk.len() + line.len() > 25000 {
                                if !current_chunk.is_empty() {
                                    chunked.push(Section {
                                        title: format!("{} [Part {}]", sec.title, part_num),
                                        body: current_chunk.clone(),
                                        line_number: sec.line_number,
                                        filename: sec.filename.clone(),
                                    });
                                    current_chunk.clear();
                                    part_num += 1;
                                }
                            }
                            if !current_chunk.is_empty() {
                                current_chunk.push('\n');
                            }
                            current_chunk.push_str(line);
                        }
                    } else {
                        current_chunk.push_str(para);
                    }
                } else {
                    if !current_chunk.is_empty() {
                        current_chunk.push_str("\n\n");
                    }
                    current_chunk.push_str(para);
                }
            }
            if !current_chunk.is_empty() {
                chunked.push(Section {
                    title: format!("{} [Part {}]", sec.title, part_num),
                    body: current_chunk,
                    line_number: sec.line_number,
                    filename: sec.filename.clone(),
                });
            }
        }
    }
    chunked
}

/// Ingests all sections into a newly initialized shivvr session and caches it
fn initialize_and_ingest_session(
    target_file: &str,
    sections: &[Section],
    corpus_size: u64,
    corpus_mtime: u64,
) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sess = format!("lume-hatcher-{}", timestamp);
    println!("\x1B[34mInitializing remote vector store session: {}...\x1B[0m", sess);

    // Ingest all sections into the remote vector store
    for (idx, sec) in sections.iter().enumerate() {
        let text = format!("Header: {}\nContent: {}", sec.title, sec.body);
        let source_str = idx.to_string();
        
        let url = format!("https://shivvr.nuts.services/temp/{}/ingest", sess);
        
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
                cleanup_session(&sess);
                delete_cached_session();
                process::exit(1);
            }
        }
    }
    println!("\x1B[32mSuccessfully ingested entire corpus into shivvr.nuts.services.\x1B[0m\n");
    save_cached_session(target_file, corpus_size, corpus_mtime, &sess);
    sess
}

pub fn run(mut args: Vec<String>) {
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
                        title: filename.clone(),
                        body: content,
                        line_number: 1,
                        filename: Some(filename),
                    });
                } else {
                    let parsed = parse_markdown(&content);
                    for mut sec in parsed {
                        sec.filename = Some(filename.clone());
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
                filename: None,
            });
        } else {
            sections = parse_markdown(&content);
        }
    }

    if sections.is_empty() {
        eprintln!("\x1B[1;31mError: No valid search sections found in corpus.\x1B[0m");
        process::exit(1);
    }

    // Automatically chunk large sections to avoid 413 Payload Too Large on shivvr
    let sections = chunk_large_sections(sections);
    println!("\x1B[32mLoaded {} sections for search corpus.\x1B[0m", sections.len());

    // Build Local BM25 Index
    println!("\x1B[34mBuilding local BM25 index...\x1B[0m");
    let bm25_index = Bm25Index::build(sections.clone(), tagger.as_ref());
    println!("\x1B[32mBM25 Index compiled successfully.\x1B[0m");

    // Read corpus metadata
    let (corpus_size, corpus_mtime) = get_corpus_metadata(path).unwrap_or((0, 0));

    // Try to load cached session
    let cached_session = load_cached_session(&target_file, corpus_size, corpus_mtime);
    let mut session_id = cached_session.unwrap_or_default();
    if !session_id.is_empty() {
        println!("\x1B[32mReusing active cached semantic session: {}\x1B[0m", session_id);
    }

    // Load local persistent semantic query-to-results cache
    let mut semantic_cache = load_semantic_cache(&target_file, corpus_size, corpus_mtime);

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
        let success = execute_hybrid_search(
            &bm25_index,
            tagger.as_ref(),
            &mut session_id,
            &target_file,
            &sections,
            corpus_size,
            corpus_mtime,
            &query,
            variant,
            &params,
            alpha,
            &mut semantic_cache,
        );
        if !success {
            println!("\x1B[33mRe-initializing remote vector session and retrying search...\x1B[0m");
            session_id = initialize_and_ingest_session(&target_file, &sections, corpus_size, corpus_mtime);
            execute_hybrid_search(
                &bm25_index,
                tagger.as_ref(),
                &mut session_id,
                &target_file,
                &sections,
                corpus_size,
                corpus_mtime,
                &query,
                variant,
                &params,
                alpha,
                &mut semantic_cache,
            );
        }
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

            let success = execute_hybrid_search(
                &bm25_index,
                tagger.as_ref(),
                &mut session_id,
                &target_file,
                &sections,
                corpus_size,
                corpus_mtime,
                query,
                variant,
                &params,
                alpha,
                &mut semantic_cache,
            );
            if !success {
                println!("\x1B[33mRe-initializing remote vector session and retrying search...\x1B[0m");
                session_id = initialize_and_ingest_session(&target_file, &sections, corpus_size, corpus_mtime);
                execute_hybrid_search(
                    &bm25_index,
                    tagger.as_ref(),
                    &mut session_id,
                    &target_file,
                    &sections,
                    corpus_size,
                    corpus_mtime,
                    query,
                    variant,
                    &params,
                    alpha,
                    &mut semantic_cache,
                );
            }
            println!();
        }
    }

    println!("\x1B[32mPreserving local semantic index. Subsequent queries for cached terms will run instantly and offline!\x1B[0m");
    println!("\x1B[1;32mHybrid Search Session closed. Thank you!\x1B[0m");
}

fn cleanup_session(session_id: &str) {
    println!("\n\x1B[34mCleaning up ephemeral remote session {}...\x1B[0m", session_id);
    let url = format!("https://shivvr.nuts.services/temp/{}", session_id);
    match ureq::delete(&url).call() {
        Ok(_) => {
            println!("\x1B[32mSuccessfully deleted remote session {}\x1B[0m", session_id);
        }
        Err(e) => {
            println!("\x1B[33mWarning: Failed to delete session: {} (it will automatically expire in 2 hours).\x1B[0m", e);
        }
    }
}

fn execute_hybrid_search(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    session_id: &mut String,
    target_file: &str,
    sections: &[Section],
    corpus_size: u64,
    corpus_mtime: u64,
    query: &str,
    variant: SearchVariant,
    params: &Bm25Params,
    alpha: f64,
    semantic_cache: &mut SemanticQueryCache,
) -> bool {
    println!("\x1B[1;34m========================================================================\x1B[0m");
    println!("\x1B[1;34m🔍  QUERY: \"{}\"\x1B[0m", query);
    println!("\x1B[1;34m========================================================================\x1B[0m");

    // --- STAGE 1: SEMANTIC VECTOR RETRIEVAL (REMOTE OR CACHED) ---
    let sem_start = Instant::now();
    let query_key = query.trim().to_lowercase();
    let mut is_cached = false;

    let semantic_results = if let Some(cached_res) = semantic_cache.queries.get(&query_key) {
        is_cached = true;
        cached_res.clone()
    } else {
        // Cache miss! Ensure remote session is initialized
        if session_id.is_empty() {
            *session_id = initialize_and_ingest_session(target_file, sections, corpus_size, corpus_mtime);
        }

        let encoded_query = percent_encode(query);
        let url = format!("https://shivvr.nuts.services/temp/{}/search?q={}&n=15", session_id, encoded_query);

        match ureq::get(&url).call() {
            Ok(res) => {
                match res.into_json::<SearchResponse>() {
                    Ok(resp) => {
                        semantic_cache.queries.insert(query_key.clone(), resp.results.clone());
                        save_semantic_cache(semantic_cache);
                        resp.results
                    }
                    Err(e) => {
                        eprintln!("\x1B[31mError parsing semantic search JSON: {}\x1B[0m", e);
                        Vec::new()
                    }
                }
            }
            Err(e) => {
                if let ureq::Error::Status(status, _) = e {
                    if status == 404 {
                        println!("\x1B[33mWarning: Remote session has expired or does not exist on server. Clearing local cache.\x1B[0m");
                        delete_cached_session();
                        session_id.clear();
                        return false;
                    }
                }
                eprintln!("\x1B[31mError querying semantic search service: {}\x1B[0m", e);
                Vec::new()
            }
        }
    };
    let sem_elapsed = sem_start.elapsed();

    if !is_cached {
        println!("\x1B[33m[DEBUG] Received {} semantic results from shivvr:\x1B[0m", semantic_results.len());
        for (i, r) in semantic_results.iter().enumerate() {
            println!("  [{}] chunk_id: {}, score: {:.4}, source: {:?}", i, r.chunk_id, r.score, r.source);
        }
    }

    // Map semantic results: section_index -> semantic_score
    let mut semantic_map: HashMap<usize, (usize, f64)> = HashMap::new();
    for (rank, res) in semantic_results.iter().enumerate() {
        if let Some(ref src) = res.source {
            if let Ok(idx) = src.parse::<usize>() {
                semantic_map.insert(idx, (rank, res.score));
            } else {
                println!("\x1B[33m[DEBUG] Failed to parse source as usize: {:?}\x1B[0m", res.source);
            }
        } else {
            println!("\x1B[33m[DEBUG] Source is None for chunk_id: {}\x1B[0m", res.chunk_id);
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
    
    // We combine the candidate pools to act as a true Set Engine Union.
    let mut candidate_indices: HashMap<usize, (f64, f64, bool)> = HashMap::new();
    
    // Add all local BM25 hits
    for hit in &bm25_hits {
        let idx = hit.section_index;
        let bm25_score = hit.score;
        candidate_indices.insert(idx, (bm25_score, 0.0, false));
    }
    
    // Merge remote semantic hits
    for (idx, (_, sem_s)) in &semantic_map {
        if let Some(entry) = candidate_indices.get_mut(idx) {
            entry.1 = *sem_s;
            entry.2 = true;
        } else {
            candidate_indices.insert(*idx, (0.0, *sem_s, true));
        }
    }

    let mut hybrid_hits: Vec<(usize, f64, f64, f64, bool)> = Vec::new();

    for (idx, (bm25_score, sem_score, boosted)) in candidate_indices {
        let hybrid_score = if bm25_score > 0.0 {
            bm25_score * (1.0 + alpha * sem_score)
        } else {
            sem_score
        };

        hybrid_hits.push((idx, bm25_score, sem_score, hybrid_score, boosted));
    }

    // Sort hybrid hits descending by hybrid score
    hybrid_hits.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    let blend_elapsed = blend_start.elapsed();

    // --- PRINT DETAILED COMPARATIVE VIEW ---
    println!("\x1B[1;32mTIMINGS:\x1B[0m");
    if is_cached {
        println!("  Remote Semantic Search (ONNX):  \x1B[1;32m[CACHED OFFLINE]\x1B[0m (returned {} docs)", semantic_results.len());
    } else {
        println!("  Remote Semantic Search (ONNX):  \x1B[36m{:.2?}\x1B[0m (returned {} docs)", sem_elapsed, semantic_results.len());
    }
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
            let title_to_show = if let Some(ref filename) = sec.filename {
                format!("{} ➔ {}", filename, sec.title)
            } else {
                sec.title.clone()
            };
            println!("  [{}] Score: \x1B[35m{:.4}\x1B[0m | \x1B[1m{}\x1B[0m", r + 1, hit.score, title_to_show);
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
                if bm25_s > 0.0 {
                    format!("\x1B[32m✨ Boosted (+{:.1}% from semantic Sim {:.4})\x1B[0m", (sem_s * alpha * 100.0), sem_s)
                } else {
                    format!("\x1B[35m✨ Semantic-Only Candidate (Sim {:.4})\x1B[0m", sem_s)
                }
            } else {
                "\x1B[31m✖ No Semantic Match (unboosted)\x1B[0m".to_string()
            };
            
            let title_to_show = if let Some(ref filename) = sec.filename {
                format!("{} ➔ {}", filename, sec.title)
            } else {
                sec.title.clone()
            };
            println!("  [{}] Hybrid Score: \x1B[1;33m{:.4}\x1B[0m (BM25: {:.4}) | \x1B[1m{}\x1B[0m", r + 1, hybrid_s, bm25_s, title_to_show);
            println!("      └─ {}", boost_indicator);
        }
    }
    println!("\x1B[1;34m========================================================================\x1B[0m\n");
    
    true
}
