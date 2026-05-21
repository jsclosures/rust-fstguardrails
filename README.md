# text-tagger

A Rust port of the `App.java` reference tagger from
<https://github.com/jsclosures/fstguardrails>. Same FST data structure,
same analyzer behaviour, same HTTP surface — but built on
[`tantivy-fst`](https://crates.io/crates/tantivy-fst) and Rust's
standard library only (no third-party HTTP crate).

## Features (Java parity)

| | Java (`App.java`) | Rust |
|---|---|---|
| FST-backed longest match (forward maximum match) | ✅ | ✅ |
| Hyphen/dash stripping (`sw-lucene` ≡ `swlucene`) | ✅ | ✅ |
| ASCII folding (`Zürich` ≡ `Zurich`) | ✅ | ✅ |
| Token separator `0x1E` between phrase tokens | ✅ | ✅ |
| Multiple records share the same FST key (synonyms emit at one span) | ✅ | ✅ |
| `Tag { start, end, surface, id, type, output }` | ✅ | ✅ |
| `output` derived as uppercase + alphanumeric, override via CSV `action` column | ✅ | ✅ |
| `DATA` env var loads every `*.csv` in a directory; filename → `type`; UUID v4 id per row | ✅ | ✅ |
| HTTP server with `GET /tag`, `POST /tag`, `GET /health` | ✅ | ✅ |
| Response formats `simple` (default) and `solr` envelope | ✅ | ✅ |
| `PORT` env var | ✅ | ✅ |
| MCP server, REPL, full demo walkthrough | ✅ | ❌ (deliberately out of scope) |

### Known limitation: ASCII folding table

The Java version uses Lucene's `ASCIIFoldingFilter`, which covers
essentially every Unicode Latin diacritic plus a long tail of folds
(ligatures, fullwidth forms, math symbols, etc.). To keep the Rust port
dependency-free, `fold_latin` in `src/lib.rs` ships a hand-rolled table
covering the common Latin-1 / Latin Extended set (À-ÿ, ß, æ, œ, þ, …).
That's enough for typical Western European text; phrases outside that
range (Polish ł, Czech č, Vietnamese ơ, etc.) currently fall through to
the separator path. Drop in a richer table — or wire up
[`deunicode`](https://crates.io/crates/deunicode) — if you need wider
coverage.

## Build & test

```bash
cd rust-text-tagger
cargo test
cargo build --release
```

## Library

```rust
use text_tagger::{Entry, Tagger};

let tagger = Tagger::build(vec![
    Entry::new("New York City", "CITY",    "geo:nyc"),
    Entry::new("Apache Lucene", "PRODUCT", "sw:lucene"),
    Entry::new("Zürich",        "CITY",    "geo:zur"),
])?;

for tag in tagger.tag("Ada uses Apache Lucene in Zurich") {
    println!("{}..{}  {}  id={}  output={}  surface={}",
        tag.start, tag.end, tag.kind, tag.id, tag.output, tag.surface);
}
```

## CLI

```bash
# TSV dictionary, args = text
cargo run --release --bin tag -- examples/dict.tsv "I love New York City"

# Or load every CSV in a directory via DATA env var
DATA=examples/data cargo run --release --bin tag -- "track my order at Apache Lucene HQ"
```

Output is one tab-separated line per tag:
`start \t end \t type \t id \t output \t surface`.

## HTTP server

```bash
# Listens on $PORT (default 8080)
DATA=examples/data cargo run --release --bin tag-server

# Simple format (default)
curl -s 'http://localhost:8080/tag?text=track+my+order+for+Apache+Lucene'
# {
#   "totaltime": 0,
#   "text": "track my order for Apache Lucene",
#   "docs": [
#     {"start":0,"end":14,"surface":"track my order","id":"<uuid>","type":"intent","output":"STATUS"},
#     {"start":19,"end":32,"surface":"Apache Lucene","id":"<uuid>","type":"product","output":"APACHELUCENE"}
#   ]
# }

# Solr envelope
curl -s 'http://localhost:8080/tag?text=zurich&format=solr'

# POST with JSON body
curl -s -X POST http://localhost:8080/tag \
     -H 'Content-Type: application/json' \
     -d '{"text":"that was total bullshit","format":"simple"}'

curl -s http://localhost:8080/health   # {"status":"ok"}
```

## Loading CSVs (`DATA` directory)

Each CSV's filename (without `.csv`) becomes the `type` for every record
from that file. The first row is the header. If a column named `action`
is present, its value becomes the record's `output` token (otherwise
`output` is derived from the phrase = uppercase + alphanumeric).

Sample data shipped in `examples/data/` is copied verbatim from the
upstream Java repo (`intent.csv`, `product.csv`, `offensive_en.csv`).

```
intent,action,response
view,VIEW,Viewing
track my order,STATUS,Tracking your order
buy,BUY,Buying
```

A row's `id` is a fresh UUID v4 (matches Java behaviour).

## How matching works

1. Input is folded (hyphen-strip → ASCII fold → lowercase) into ASCII
   tokens with original byte offsets preserved.
2. From each token cursor `i`, the matcher walks the FST byte by byte;
   the inter-token separator `0x1E` is fed between tokens.
3. Every visit to a final state records a match. The longest match wins
   (Java's forward-maximum-match), and overlapping shorter matches are
   skipped. When multiple records share the matched FST key (synonyms),
   the tagger emits one `Tag` per record at the same span.
