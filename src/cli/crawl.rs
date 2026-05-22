use std::fs;
use std::io::{self, Write};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CrawlPayload<'a> {
    url: &'a str,
    javascript_enabled: bool,
}

#[derive(Deserialize)]
struct CrawlResponse {
    markdown: Option<String>,
    markdown_plain: Option<String>,
    content: Option<String>,
    title: Option<String>,
    error: Option<String>,
}

pub fn run(args: Vec<String>) {
    if args.is_empty() {
        println!("\x1B[1;31mError: No URL provided.\x1B[0m");
        println!("\x1B[1;33mUsage:\x1B[0m");
        println!("  lume crawl <URL>");
        println!();
        println!("\x1B[1;33mExample:\x1B[0m");
        println!("  lume crawl https://example.com");
        return;
    }

    let url = &args[0];

    // Load nuts.services token
    let token = match load_nuts_token() {
        Some(tok) => tok,
        None => {
            eprintln!("\x1B[1;31mError: nuts.services token not found.\x1B[0m");
            eprintln!("\x1B[33mPlease get a token at https://nuts.services and set it in your .env file:\x1B[0m");
            eprintln!("  NUTS_SERVICES_TOKEN=your_token_here");
            std::process::exit(1);
        }
    };

    println!("\x1B[1;36m🕷️  Lume Crawler starting for: {}\x1B[0m", url);
    println!("  ➔ Dispatching stealth crawl agent to grub.nuts.services...");
    io::stdout().flush().unwrap();

    let api_url = std::env::var("GRUB_BASE_URL")
        .unwrap_or_else(|_| "https://grub.nuts.services".to_string());
    let endpoint = format!("{}/api/markdown", api_url.trim_end_matches('/'));

    let payload = CrawlPayload {
        url,
        javascript_enabled: false,
    };

    let start = std::time::Instant::now();

    // Send HTTP POST request via ureq (set long timeout e.g. 60 seconds since crawling might take time)
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build();

    let auth_header = format!("Bearer {}", token);

    match agent.post(&endpoint)
        .set("Content-Type", "application/json")
        .set("Authorization", &auth_header)
        .send_json(&payload)
    {
        Ok(res) => {
            let status = res.status();
            if status != 200 {
                let err_body = res.into_string().unwrap_or_else(|_| "Unknown error".to_string());
                eprintln!("\x1B[1;31mCrawl failed (Status {}):\x1B[0m {}", status, err_body);
                std::process::exit(1);
            }

            let response_data: CrawlResponse = match res.into_json() {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("\x1B[1;31mFailed to parse response JSON:\x1B[0m {}", e);
                    std::process::exit(1);
                }
            };

            if let Some(err) = response_data.error {
                eprintln!("\x1B[1;31mCrawl server error:\x1B[0m {}", err);
                std::process::exit(1);
            }

            // Extract the markdown content
            let markdown = response_data.markdown
                .or(response_data.markdown_plain)
                .or(response_data.content);

            match markdown {
                Some(content) => {
                    let elapsed = start.elapsed();
                    println!("\x1B[1;32m✓ Crawled successfully in {:.2?}!\x1B[0m", elapsed);

                    // Save to personal search engine (examples/crawled/)
                    let save_dir = "examples/crawled";
                    if let Err(e) = fs::create_dir_all(save_dir) {
                        eprintln!("\x1B[1;31mFailed to create directory '{}':\x1B[0m {}", save_dir, e);
                        std::process::exit(1);
                    }

                    // Create slug
                    let safe_slug = make_safe_slug(url);
                    let filename = format!("{}/{}.md", save_dir, safe_slug);
                    
                    let page_title = response_data.title.unwrap_or_else(|| "Crawled Document".to_string());

                    // Prep file content with title and source URL header
                    let file_content = format!(
                        "# {}\n\n*   **Source URL**: {}\n*   **Crawl Timestamp**: {}\n\n---\n\n{}",
                        page_title,
                        url,
                        chrono_timestamp(),
                        content
                    );

                    match fs::write(&filename, file_content) {
                        Ok(_) => {
                            println!(
                                "\x1B[32mSuccessfully added to personal search engine document collection!\x1B[0m"
                            );
                            println!("  ➔ Saved to: \x1B[1;34m{}\x1B[0m", filename);
                            println!("  ➔ You can now search it immediately using: \x1B[1;36mlume search examples/crawled \"your query\"\x1B[0m");
                        }
                        Err(e) => {
                            eprintln!("\x1B[1;31mFailed to write markdown file to '{}':\x1B[0m {}", filename, e);
                            std::process::exit(1);
                        }
                    }
                }
                None => {
                    eprintln!("\x1B[1;31mCrawl returned empty content.\x1B[0m");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("\x1B[1;31mConnection error:\x1B[0m {}", e);
            std::process::exit(1);
        }
    }
}

fn load_nuts_token() -> Option<String> {
    // 1. Check environment variable
    if let Ok(tok) = std::env::var("NUTS_SERVICES_TOKEN") {
        return Some(tok.trim().to_string());
    }
    // 2. Read .env file in current directory
    if let Ok(content) = fs::read_to_string(".env") {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("NUTS_SERVICES_TOKEN=") {
                let parts: Vec<&str> = line.splitn(2, '=').collect();
                if parts.len() == 2 {
                    return Some(parts[1].trim().to_string());
                }
            }
        }
    }
    None
}

fn make_safe_slug(url: &str) -> String {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let mut slug = String::new();
    for c in stripped.chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if c == '/' || c == '?' || c == '&' || c == '=' || c == '-' || c == '_' || c == '.' {
            slug.push('_');
        }
    }
    // Remove repeated underscores
    let mut cleaned = String::new();
    let mut last_was_underscore = false;
    for c in slug.chars() {
        if c == '_' {
            if !last_was_underscore {
                cleaned.push('_');
                last_was_underscore = true;
            }
        } else {
            cleaned.push(c);
            last_was_underscore = false;
        }
    }
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "index".to_string()
    } else {
        trimmed.to_string()
    }
}

fn chrono_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            format!("Unix Epoch Secs {}", secs)
        }
        Err(_) => "Unknown time".to_string(),
    }
}
