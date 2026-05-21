//! CLI: `tag [dictionary.tsv] [text...]`
//!
//! Loading order:
//!   1. If `DATA` is set, load every `*.csv` file in that directory; each
//!      filename (sans `.csv`) becomes the entry `kind`, and an `action`
//!      column (if present) becomes each record's `output`.
//!   2. Otherwise, the first positional arg must be a `.tsv` dictionary.
//!
//! Output: one tab-separated line per tag —
//!     start \t end \t kind \t id \t output \t surface

use std::env;
use std::io::{self, Read};
use std::process;

use text_tagger::Tagger;

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();

    let tagger = match Tagger::from_env() {
        Ok(Some(t)) => {
            eprintln!(
                "loaded {} records ({} keys) from DATA={} (kinds: {})",
                t.record_count(),
                t.len(),
                env::var("DATA").unwrap_or_default(),
                t.kinds().join(", ")
            );
            t
        }
        Ok(None) => {
            if args.is_empty() {
                eprintln!("usage: tag <dictionary.tsv> [text...]   (or set DATA=<csv dir>)");
                process::exit(2);
            }
            let dict = args.remove(0);
            match Tagger::from_tsv_file(&dict) {
                Ok(t) => {
                    eprintln!("loaded {} records from {}", t.record_count(), dict);
                    t
                }
                Err(e) => {
                    eprintln!("failed to load {dict}: {e}");
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("failed to load DATA dir: {e}");
            process::exit(1);
        }
    };

    let text = if args.is_empty() {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).expect("read stdin");
        s
    } else {
        args.join(" ")
    };

    for tag in tagger.tag(&text) {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            tag.start, tag.end, tag.kind, tag.id, tag.output, tag.surface
        );
    }
}
