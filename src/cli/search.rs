use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::time::Instant;

use crate::bm25::{parse_markdown, Bm25Index, Bm25Params, SearchVariant};
use crate::{tokenize, Tagger};

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

pub fn run(mut args: Vec<String>) {
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
    let doc_name = std::path::Path::new(&md_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");
    
    let path = std::path::Path::new(&md_path);
    let is_directory = path.is_dir();
    let is_csv = !is_directory && md_path.to_ascii_lowercase().ends_with(".csv");

    // Index Document
    let start_indexing = Instant::now();
    let sections = if is_directory {
        let mut files = Vec::new();
        if let Err(e) = collect_files(path, &mut files) {
            eprintln!("\x1B[1;31mFailed to read directory {md_path}:\x1B[0m {e}");
            process::exit(1);
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
                    eprintln!("\x1B[1;33mWarning: Failed to read file {:?}: {}\x1B[0m", file_path, e);
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
                dir_sections.push(crate::bm25::Section {
                    title: filename.clone(),
                    body: content,
                    line_number: 1,
                    filename: Some(filename),
                });
            }
        }
        dir_sections
    } else if is_csv {
        let md_content = match fs::read_to_string(&md_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("\x1B[1;31mFailed to read CSV document {md_path}:\x1B[0m {e}");
                process::exit(1);
            }
        };
        // Parse CSV into sections dynamically
        let mut csv_sections = Vec::new();
        let mut lines = md_content.lines();
        if let Some(header_line) = lines.next() {
            let headers = crate::parse_csv_line(header_line);
            
            // Try to find a good column to use as a title/key (e.g., "name" or "phrase" or "title" or "id")
            let title_col_idx = headers.iter()
                .position(|h| {
                    let h_lower = h.trim().to_lowercase();
                    h_lower == "name" || h_lower == "phrase" || h_lower == "title" || h_lower == "id"
                })
                .unwrap_or(0);

            for (i, line) in lines.enumerate() {
                let line_num = i + 2; // header is line 1, rows start at 2
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
                
                // Formulate a detailed and highly searchable body representing all fields in key-value format
                let mut body_parts = Vec::new();
                for (col_idx, value) in cells.iter().enumerate() {
                    let col_name = headers.get(col_idx).map(|s| s.trim().as_ref()).unwrap_or("column");
                    body_parts.push(format!("{}: {}", col_name, value.trim()));
                }
                let body = body_parts.join(" | ");
                
                csv_sections.push(crate::bm25::Section {
                    title,
                    body,
                    line_number: line_num,
                    filename: None,
                });
            }
        }
        csv_sections
    } else {
        let md_content = match fs::read_to_string(&md_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("\x1B[1;31mFailed to read Markdown document {md_path}:\x1B[0m {e}");
                process::exit(1);
            }
        };
        parse_markdown(&md_content)
    };

    let index = Bm25Index::build(sections, tagger.as_ref());
    let index_time = start_indexing.elapsed();
    let doc_type_str = if is_directory {
        "directory sections"
    } else if is_csv {
        "CSV rows"
    } else {
        "Markdown sections"
    };
    eprintln!(
        "\x1B[32mIndexed {} {} in {:.2?}\x1B[0m",
        index.num_docs, doc_type_str, index_time
    );

    // Build Spelling Index
    let start_spelling = Instant::now();
    let fst_phrases = tagger.as_ref().map(|t| t.phrases().to_vec()).unwrap_or_default();
    let corpus_terms: Vec<Vec<u8>> = index.posting_lists.keys().cloned().collect();
    let spell_index = crate::spelling::SpellIndex::build(&fst_phrases, &corpus_terms);
    let spelling_time = start_spelling.elapsed();
    eprintln!(
        "\x1B[32mCompiled roaring spelling index with {} vocabulary words in {:.2?}\x1B[0m",
        spell_index.num_words, spelling_time
    );

    // If query terms are passed as args, check for specialized commands or execute one-shot search
    if !args.is_empty() {
        let cmd = args[0].trim().to_lowercase();
        if cmd == "graph" {
            let min_similarity = args.get(1).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.02);
            execute_graph(&index, min_similarity);
        } else if cmd == "generate" {
            let seed = args.get(1).map(|s| s.as_str());
            execute_generate(&index, tagger.as_ref(), seed, doc_name, is_csv);
        } else {
            let query = args.join(" ");
            execute_search(&index, tagger.as_ref(), &spell_index, &query, variant, &params);
        }
    } else {
        // Run Interactive REPL loop
        run_repl(&index, tagger.as_ref(), &spell_index, variant, &params, doc_name, is_csv);
    }
}

fn execute_graph(index: &Bm25Index, min_similarity: f64) {
    use crate::semantic_mesh::EntityGraph;
    
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

fn execute_generate(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    seed: Option<&str>,
    doc_name: &str,
    is_csv: bool,
) {
    use crate::semantic_mesh::MarkovChain;
    
    println!("\x1B[1;34mBuilding Trigram Markov Chain Model...\x1B[0m");
    let start_build = Instant::now();
    let bodies: Vec<&str> = index.sections.iter().map(|s| s.body.as_str()).collect();
    let chain = MarkovChain::build(&bodies);
    let build_elapsed = start_build.elapsed();
    println!("\x1B[32mBuilt Markov Chain ({} transition keys) in {:.2?}\x1B[0m", chain.transitions.len(), build_elapsed);
    
    if is_csv {
        println!("\x1B[1;34mGenerating simulated CSV records...\x1B[0m");
    } else {
        println!("\x1B[1;34mGenerating passage in the style of {} (Guided Local Attention)...\x1B[0m", doc_name);
    }
    let start_gen = Instant::now();
    let (text, attention_history) = chain.generate_steered(
        seed,
        150,
        tagger,
        &index.entity_posting_lists,
    );
    let gen_elapsed = start_gen.elapsed();
    
    println!();
    println!("\x1B[1;36m\"{}\"\x1B[0m", text);
    println!();
    
    // Print dynamic attention traces if we had active FST tags!
    if !attention_history.is_empty() {
        println!("\x1B[1;35m--- 🧠 FST ATTENTION FEEDBACK TRACES ---\x1B[0m");
        let mut last_printed_token = 0;
        for (token_idx, register) in &attention_history {
            if token_idx - last_printed_token >= 8 {
                let mut trace_strs = Vec::new();
                for (tag, weight) in register {
                    trace_strs.push(format!("\x1B[1;33m{}\x1B[0m ({:.2})", tag, weight));
                }
                println!("  Token #{:3}: [Active Attention: {}]", token_idx, trace_strs.join(", "));
                last_printed_token = *token_idx;
            }
        }
        println!("\x1B[1;35m----------------------------------------\x1B[0m\n");
    }
    
    let gen_type = if is_csv { "simulated records" } else { "passage" };
    println!("\x1B[32mGenerated {} in {:.2?}\x1B[0m", gen_type, gen_elapsed);
}

fn execute_search(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    spell_index: &crate::spelling::SpellIndex,
    query: &str,
    variant: SearchVariant,
    params: &Bm25Params,
) {
    eprintln!("\x1B[1;34mSearching for:\x1B[0m \"{}\" (Variant: {:?})", query, variant);
    
    // 0. spelling correction check
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
        eprintln!("  \x1B[1;33m💡 Did you mean:\x1B[0m \"\x1B[1;4m{}\x1B[0m\" ? (corrected from \"{}\")", corrected_query, query);
        for q_tok in &query_tokens {
            if let Ok(q_word) = String::from_utf8(q_tok.bytes.clone()) {
                let q_word_lower = q_word.to_lowercase();
                if q_word_lower.chars().any(|c| c.is_alphabetic()) && !spell_index.vocab_set.contains(&q_word_lower) {
                    let pattern = crate::regex::levenshtein_regex(&q_word_lower);
                    eprintln!("     \x1B[35m└─ Thompson NFA Levenshtein Expansion:\x1B[0m \"{}\" ➔ regex AST: \x1B[1m\"{}\"\x1B[0m", q_word, pattern);
                }
            }
        }
    }
    
    // Tag query itself with FST if enabled
    if let Some(ref t) = tagger {
        let query_tags = t.tag(query);
        if !query_tags.is_empty() {
            eprint!("  \x1B[32m└─ Matched Query Entities:\x1B[0m ");
            for (idx, tag) in query_tags.iter().enumerate() {
                if idx > 0 {
                    eprint!(", ");
                }
                eprint!("\x1B[1m{}\x1B[0m [entity={}, type={}]", tag.surface, tag.output, tag.kind);
            }
            eprintln!();
        }
    }

    // Pairwise Jaccard index between query terms' posting lists
    let query_tokens = tokenize(query);
    if query_tokens.len() > 1 {
        eprintln!("  \x1B[35m└─ Query Term Posting List Jaccard Similarities:\x1B[0m");
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
                        eprintln!("     - '{}' vs '{}': {:.4} (Intersection: {}, Union: {})", 
                            str_a, str_b, jaccard, la.intersect(lb).len(), la.union(lb).len());
                    }
                    _ => {
                        eprintln!("     - '{}' vs '{}': 0.0000 (One or both terms not found)", str_a, str_b);
                    }
                }
            }
        }
    }

    let start_search = Instant::now();
    let hits = index.search(query, variant, params, tagger);
    let elapsed = start_search.elapsed();

    eprintln!("\x1B[34mFound {} ranked results in {:.2?}\x1B[0m\n", hits.len(), elapsed);

    for (rank, hit) in hits.iter().enumerate() {
        let section = &index.sections[hit.section_index];
        println!(
            "\x1B[1;35mRank {} | Score: {:.4}\x1B[0m",
            rank + 1, hit.score
        );
        let title_to_show = if let Some(ref filename) = section.filename {
            format!("{} ➔ {}", filename, section.title)
        } else {
            section.title.clone()
        };
        println!(
            "\x1B[1;36mHeader: {} (Line {})\x1B[0m",
            title_to_show, section.line_number
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
                    label: format!("{} ({})", tag.output, tag.kind),
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
    spell_index: &crate::spelling::SpellIndex,
    variant: SearchVariant,
    params: &Bm25Params,
    doc_name: &str,
    is_csv: bool,
) {
    println!();
    println!("      \x1B[1;36m▄▀▀▄        Antigravity Search Mesh REPL\x1B[0m");
    println!("     \x1B[36m▀▀▀▀▀▀       Field-Aware BM25 + FST Entity Tagger\x1B[0m");
    println!("    \x1B[1;34m▀▀▀▀▀▀▀▀      Zero External Dependencies (StdLib Only)\x1B[0m");
    println!("────────────────────────────────────────────────────────────");
    println!("Commands:");
    println!("  - Type a query to search the BM25 FST mesh.");
    println!("  - Type \x1B[1mchat\x1B[0m to enter interactive Q&A AI mode.");
    println!("  - Type \x1B[1mgraph [min_sim]\x1B[0m to compute entity graph & write JSON.");
    if is_csv {
        println!("  - Type \x1B[1mgenerate [seed]\x1B[0m to generate simulated CSV records based on the dataset.");
    } else {
        println!("  - Type \x1B[1mgenerate [seed]\x1B[0m to generate text in the style of {}.", doc_name);
    }
    println!("  - Type \x1B[1mexit\x1B[0m or \x1B[1mquit\x1B[0m to end.");
    println!();

    let mut stdout = io::stdout();

    loop {
        print!("\x1B[1;32msearch > \x1B[0m");
        let _ = stdout.flush();

        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
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
                execute_generate(index, tagger, seed, doc_name, is_csv);
                println!();
                continue;
            } else if cmd == "chat" {
                run_chat_mode(index, tagger, spell_index, variant, params, doc_name, is_csv);
                println!();
                continue;
            }
        }

        execute_search(index, tagger, spell_index, query, variant, params);
        println!();
    }
}

fn get_ai_name(doc_name: &str, is_csv: bool) -> String {
    let clean = doc_name.replace('_', " ").replace('-', " ");
    let mut capitalized = String::new();
    for word in clean.split_whitespace() {
        if !capitalized.is_empty() {
            capitalized.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            for c in first.to_uppercase() {
                capitalized.push(c);
            }
            capitalized.push_str(chars.as_str());
        }
    }
    if capitalized.is_empty() {
        if is_csv {
            "Data AI".to_string()
        } else {
            "Document AI".to_string()
        }
    } else {
        format!("{} AI", capitalized)
    }
}

fn run_chat_mode(
    index: &Bm25Index,
    tagger: Option<&Tagger>,
    _spell_index: &crate::spelling::SpellIndex,
    variant: SearchVariant,
    params: &Bm25Params,
    doc_name: &str,
    is_csv: bool,
) {
    let ai_name = get_ai_name(doc_name, is_csv);
    println!();
    if is_csv {
        println!("      \x1B[1;36m🤖 {} Chat Mode Activated\x1B[0m", ai_name);
        println!("     \x1B[36mAsk questions or query specific records in the dataset.\x1B[0m");
    } else {
        println!("      \x1B[1;36m🤖 {} Chat Mode Activated\x1B[0m", ai_name);
        println!("     \x1B[36mConversing in the style of {}.\x1B[0m", doc_name);
    }
    println!("────────────────────────────────────────────────────────────");
    println!("Type \x1B[1mexit\x1B[0m or \x1B[1mquit\x1B[0m to return to standard search.");
    println!();

    let mut stdout = io::stdout();
    use crate::semantic_mesh::MarkovChain;
    
    let bodies: Vec<&str> = index.sections.iter().map(|s| s.body.as_str()).collect();
    let chain = MarkovChain::build(&bodies);

    loop {
        print!("\x1B[1;35mchat ({}) > \x1B[0m", doc_name.to_lowercase());
        let _ = stdout.flush();

        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }

        let query = line.trim();
        if query.is_empty() {
            continue;
        }
        if query == "exit" || query == "quit" || query == "back" {
            println!("\x1B[1;34mReturning to standard search mode.\x1B[0m");
            break;
        }

        let hits = index.search(query, variant, params, tagger);
        if hits.is_empty() {
            println!();
            if is_csv {
                println!("  \x1B[1;31m📊 [{}]:\x1B[0m \"Alas, I found no records matching '{}' in our system.\"", ai_name, query);
            } else {
                println!("  \x1B[1;31m🤖 [{}]:\x1B[0m \"Alas, my friend, my pages hold no record of '{}'. Tell me, what else shall we speak of?\"", ai_name, query);
            }
            println!();
            continue;
        }

        let top_hit = &hits[0];
        let section = &index.sections[top_hit.section_index];
        let query_tokens = crate::tokenize(query);

        println!();
        if is_csv {
            println!("  \x1B[1;36m📊 [{}]:\x1B[0m \"I located a matching record for you!\"", ai_name);
            let title_to_show = if let Some(ref filename) = section.filename {
                format!("{} ➔ {}", filename, section.title)
            } else {
                section.title.clone()
            };
            println!("     \x1B[1;34m📍 Key/Title:\x1B[0m {}", title_to_show);
            println!("     \x1B[1;34m📋 Record Fields:\x1B[0m");
            for field in section.body.split(" | ") {
                println!("        - {}", field);
            }
            
            let seed = query_tokens.first().map(|t| String::from_utf8_lossy(&t.bytes).to_string());
            let (simulated, _) = chain.generate_steered(
                seed.as_deref(),
                30,
                tagger,
                &index.entity_posting_lists,
            );
            println!("     \x1B[1;33m💡 Simulated Pattern Row:\x1B[0m");
            println!("        {}", simulated);
        } else {
            let mut start_idx = 0;
            let body = &section.body;
            if !query_tokens.is_empty() {
                let first_term = String::from_utf8_lossy(&query_tokens[0].bytes).to_lowercase();
                if let Some(pos) = body.to_lowercase().find(&first_term) {
                    start_idx = if pos > 50 { pos - 50 } else { 0 };
                    while start_idx > 0 && !body.is_char_boundary(start_idx) {
                        start_idx -= 1;
                    }
                }
            }
            
            let end_idx = (start_idx + 250).min(body.len());
            let snippet_slice = &body[start_idx..end_idx];
            let prefix = if start_idx > 0 { "... " } else { "" };
            let suffix = if end_idx < body.len() { " ..." } else { "" };
            
            let title_to_show = if let Some(ref filename) = section.filename {
                format!("{} ➔ {}", filename, section.title)
            } else {
                section.title.clone()
            };
            println!("  \x1B[1;36m🤖 [{}]:\x1B[0m \"Ah, yes! Let us speak of \x1B[1m{}\x1B[0m. In our chronicle, it is written:\"", ai_name, title_to_show);
            println!("     \x1B[3m\"{}{}{}\"\x1B[0m", prefix, snippet_slice.trim(), suffix);
            
            let seed = query_tokens.first().map(|t| String::from_utf8_lossy(&t.bytes).to_string());
            let (continuation, attention_history) = chain.generate_steered(
                seed.as_deref(),
                80,
                tagger,
                &index.entity_posting_lists,
            );
            println!();
            
            let doc_title = get_ai_name(doc_name, false);
            let continuation_label = if doc_title.ends_with(" AI") {
                &doc_title[..doc_title.len() - 3]
            } else {
                &doc_title
            };
            println!("     \x1B[1;34m[{} continues...]\x1B[0m", continuation_label);
            println!("     \x1B[32m\"{}\"\x1B[0m", continuation);
            
            if !attention_history.is_empty() {
                println!();
                println!("     \x1B[1;35m--- 🧠 FST ATTENTION FEEDBACK TRACES ---\x1B[0m");
                let mut last_printed_token = 0;
                for (token_idx, register) in &attention_history {
                    if token_idx - last_printed_token >= 8 {
                        let mut trace_strs = Vec::new();
                        for (tag, weight) in register {
                            trace_strs.push(format!("\x1B[1;33m{}\x1B[0m ({:.2})", tag, weight));
                        }
                        println!("       Token #{:3}: [Active Attention: {}]", token_idx, trace_strs.join(", "));
                        last_printed_token = *token_idx;
                    }
                }
                println!("     \x1B[1;35m----------------------------------------\x1B[0m");
            }
        }
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
