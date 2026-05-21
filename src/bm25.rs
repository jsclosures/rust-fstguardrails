use std::collections::HashMap;
use crate::tokenize;

/// Represents a section parsed from a Markdown document.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub body: String,
    pub line_number: usize,
}

/// The three BM25 variants supported by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchVariant {
    Classic,
    Plus,
    L,
}

/// Tuning parameters and field weights for the field-aware BM25 engine.
#[derive(Debug, Clone)]
pub struct Bm25Params {
    pub k1: f64,
    pub b: f64,
    pub delta: f64, // Used for BM25+
    pub title_weight: f64,
    pub body_weight: f64,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            delta: 1.0,
            title_weight: 2.0,
            body_weight: 1.0,
        }
    }
}

/// A parsed, in-memory index of Markdown sections.
#[derive(Debug, Clone)]
pub struct Bm25Index {
    pub sections: Vec<Section>,
    pub num_docs: usize,
    
    // Per-document term frequency maps for each field (indexed by token bytes)
    pub title_tfs: Vec<HashMap<Vec<u8>, usize>>,
    pub body_tfs: Vec<HashMap<Vec<u8>, usize>>,
    
    // Total token counts per document field
    pub title_lens: Vec<usize>,
    pub body_lens: Vec<usize>,
    
    // Average field lengths across the corpus
    pub avg_title_len: f64,
    pub avg_body_len: f64,
    
    // Corpus-wide document frequencies: token bytes -> number of docs containing it
    pub title_dfs: HashMap<Vec<u8>, usize>,
    pub body_dfs: HashMap<Vec<u8>, usize>,
}

/// A hit returned by the search query.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub section_index: usize,
    pub score: f64,
}

/// Simple, robust line-by-line Markdown section parser.
/// Cuts sections at `#` headers and records their starting line numbers.
pub fn parse_markdown(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_title = String::from("Introduction");
    let mut current_body = Vec::new();
    let mut start_line = 1;

    for (i, line) in content.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let hashes_count = trimmed.chars().take_while(|&c| c == '#').count();
            let header_text = trimmed[hashes_count..].trim().to_string();
            
            if hashes_count > 0 && !header_text.is_empty() {
                // Save previous section if it has any content
                let body_text = current_body.join("\n");
                sections.push(Section {
                    title: current_title,
                    body: body_text,
                    line_number: start_line,
                });
                
                current_title = header_text;
                current_body.clear();
                start_line = line_num;
                continue;
            }
        }
        current_body.push(line.to_string());
    }

    // Push the final section
    let body_text = current_body.join("\n");
    sections.push(Section {
        title: current_title,
        body: body_text,
        line_number: start_line,
    });

    // Retain sections that aren't completely blank
    sections.retain(|s| !s.title.trim().is_empty() || !s.body.trim().is_empty());
    sections
}

impl Bm25Index {
    /// Constructs a search index over a collection of Markdown sections.
    pub fn build(sections: Vec<Section>) -> Self {
        let num_docs = sections.len();
        let mut title_tfs = Vec::with_capacity(num_docs);
        let mut body_tfs = Vec::with_capacity(num_docs);
        let mut title_lens = Vec::with_capacity(num_docs);
        let mut body_lens = Vec::with_capacity(num_docs);
        
        let mut title_dfs = HashMap::new();
        let mut body_dfs = HashMap::new();
        
        let mut total_title_len = 0;
        let mut total_body_len = 0;

        for sec in &sections {
            let t_toks = tokenize(&sec.title);
            let b_toks = tokenize(&sec.body);
            
            title_lens.push(t_toks.len());
            body_lens.push(b_toks.len());
            total_title_len += t_toks.len();
            total_body_len += b_toks.len();
            
            // Build Title TF
            let mut t_tf = HashMap::new();
            for tok in t_toks {
                *t_tf.entry(tok.bytes).or_insert(0) += 1;
            }
            for tok_bytes in t_tf.keys() {
                *title_dfs.entry(tok_bytes.clone()).or_insert(0) += 1;
            }
            title_tfs.push(t_tf);
            
            // Build Body TF
            let mut b_tf = HashMap::new();
            for tok in b_toks {
                *b_tf.entry(tok.bytes).or_insert(0) += 1;
            }
            for tok_bytes in b_tf.keys() {
                *body_dfs.entry(tok_bytes.clone()).or_insert(0) += 1;
            }
            body_tfs.push(b_tf);
        }
        
        let avg_title_len = if num_docs > 0 {
            total_title_len as f64 / num_docs as f64
        } else {
            0.0
        };
        
        let avg_body_len = if num_docs > 0 {
            total_body_len as f64 / num_docs as f64
        } else {
            0.0
        };

        Self {
            sections,
            num_docs,
            title_tfs,
            body_tfs,
            title_lens,
            body_lens,
            avg_title_len,
            avg_body_len,
            title_dfs,
            body_dfs,
        }
    }

    /// Evaluates a query and returns matching sections ordered by their BM25 score.
    pub fn search(
        &self,
        query: &str,
        variant: SearchVariant,
        params: &Bm25Params,
    ) -> Vec<SearchHit> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.num_docs == 0 {
            return Vec::new();
        }
        
        let mut hits = Vec::new();
        
        for doc_idx in 0..self.num_docs {
            let mut total_score = 0.0;
            
            for q_tok in &query_tokens {
                let tok_bytes = &q_tok.bytes;
                
                // 1. Title Contribution
                let title_score = {
                    let tf = self.title_tfs[doc_idx].get(tok_bytes).copied().unwrap_or(0) as f64;
                    let df = self.title_dfs.get(tok_bytes).copied().unwrap_or(0);
                    
                    let idf = ((self.num_docs as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
                    let idf = idf.max(0.0);
                    
                    let doc_len = self.title_lens[doc_idx] as f64;
                    let avgdl = self.avg_title_len;
                    
                    calculate_bm25_term_score(tf, doc_len, avgdl, idf, variant, params)
                };
                
                // 2. Body Contribution
                let body_score = {
                    let tf = self.body_tfs[doc_idx].get(tok_bytes).copied().unwrap_or(0) as f64;
                    let df = self.body_dfs.get(tok_bytes).copied().unwrap_or(0);
                    
                    let idf = ((self.num_docs as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
                    let idf = idf.max(0.0);
                    
                    let doc_len = self.body_lens[doc_idx] as f64;
                    let avgdl = self.avg_body_len;
                    
                    calculate_bm25_term_score(tf, doc_len, avgdl, idf, variant, params)
                };
                
                total_score += params.title_weight * title_score + params.body_weight * body_score;
            }
            
            if total_score > 0.0 {
                hits.push(SearchHit {
                    section_index: doc_idx,
                    score: total_score,
                });
            }
        }
        
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits
    }
}

/// Helper function to perform BM25 variant score calculation.
fn calculate_bm25_term_score(
    tf: f64,
    doc_len: f64,
    avgdl: f64,
    idf: f64,
    variant: SearchVariant,
    params: &Bm25Params,
) -> f64 {
    if tf == 0.0 {
        return 0.0;
    }
    
    let k1 = params.k1;
    let b = params.b;
    
    let len_normalization = if avgdl > 0.0 {
        1.0 - b + b * (doc_len / avgdl)
    } else {
        1.0
    };
    
    match variant {
        SearchVariant::Classic => {
            idf * (tf * (k1 + 1.0)) / (tf + k1 * len_normalization)
        }
        SearchVariant::Plus => {
            let term_tf_score = (tf * (k1 + 1.0)) / (tf + k1 * len_normalization);
            idf * (term_tf_score + params.delta)
        }
        SearchVariant::L => {
            let scaled_tf = tf / len_normalization;
            idf * (scaled_tf * (k1 + 1.0)) / (scaled_tf + k1)
        }
    }
}
