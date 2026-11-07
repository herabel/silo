use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

pub fn find_web_scout_bin() -> Option<PathBuf> {
    // 1. Next to current executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let bin_exe = parent.join("web_scout.exe");
            if bin_exe.exists() {
                return Some(bin_exe);
            }
            let bin = parent.join("web_scout");
            if bin.exists() {
                return Some(bin);
            }
        }
    }

    // 2. In current working directory
    let cwd_exe = PathBuf::from("web_scout.exe");
    if cwd_exe.exists() {
        return Some(cwd_exe);
    }
    let cwd_bin = PathBuf::from("web_scout");
    if cwd_bin.exists() {
        return Some(cwd_bin);
    }

    // 3. Search system PATH
    if let Ok(paths) = std::env::var("PATH") {
        for path in std::env::split_paths(&paths) {
            #[cfg(windows)]
            {
                let candidate = path.join("web_scout.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            #[cfg(not(windows))]
            {
                let candidate = path.join("web_scout");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

pub async fn run_python_code(code: &str) -> (bool, String) {
    let mut clean_code = code.trim().to_string();

    // Strip markdown code fences if model wrapped code in ```python ... ```
    if clean_code.starts_with("```") {
        if let Some(first_nl) = clean_code.find('\n') {
            clean_code = clean_code[first_nl + 1..].to_string();
        }
        if let Some(last_fence) = clean_code.rfind("```") {
            clean_code = clean_code[..last_fence].to_string();
        }
        clean_code = clean_code.trim().to_string();
    }

    // Strip leading "code:" or "python:" prefixes or pseudo-tags if model emitted pseudo-labels
    let lower_trim = clean_code.to_lowercase();
    if lower_trim.starts_with("code:") || lower_trim.starts_with("code :") {
        clean_code = clean_code[5..].trim().to_string();
    } else if lower_trim.starts_with("python:") || lower_trim.starts_with("python :") {
        clean_code = clean_code[7..].trim().to_string();
    } else if lower_trim.starts_with("code>") {
        clean_code = clean_code[5..].trim().to_string();
    } else if lower_trim.starts_with("python>") {
        clean_code = clean_code[7..].trim().to_string();
    }

    if let Ok(tag_re) = regex::Regex::new(r#"^(?:</?[a-zA-Z0-9_-]+[^>]*>)\s*"#) {
        clean_code = tag_re.replace(&clean_code, "").trim().to_string();
    }
    if let Ok(tag_re) = regex::Regex::new(r#"\s*</[a-zA-Z0-9_-]+>$"#) {
        clean_code = tag_re.replace(&clean_code, "").trim().to_string();
    }

    // ONLY unescape \n if the code is entirely on a single line and has literal \n
    // (Never replace \n in multiline code, as that breaks valid string literals like print("...\n...") into raw newlines!)
    if !clean_code.contains('\n') && clean_code.contains("\\n") {
        clean_code = clean_code.replace("\\r\\n", "\n").replace("\\n", "\n");
    }
    if !clean_code.contains('\t') && clean_code.contains("\\t") {
        clean_code = clean_code.replace("\\t", "\t");
    }

    // Auto-wrap single line expression without print in print(...)
    if !clean_code.contains('\n')
        && !clean_code.is_empty()
        && !clean_code.starts_with("import")
        && !clean_code.starts_with("from")
        && !clean_code.starts_with("print")
        && !clean_code.starts_with("def ")
        && !clean_code.starts_with("class ")
        && !clean_code.starts_with("for ")
        && !clean_code.starts_with("while ")
        && !clean_code.starts_with("if ")
        && !clean_code.starts_with("with ")
        && !clean_code.contains('=')
    {
        clean_code = format!("print({})", clean_code);
    }

    let mut cmd = Command::new("python");
    cmd.env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        // 0x08000000 = CREATE_NO_WINDOW
        cmd.creation_flags(0x08000000);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                format!("[Execution Error]: Failed to spawn python: {}", e),
            )
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(clean_code.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    }

    let wait_res = timeout(Duration::from_secs(25), child.wait()).await;
    match wait_res {
        Ok(Ok(status)) => {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut stdout_buf).await;
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_end(&mut stderr_buf).await;
            }

            let stdout = String::from_utf8_lossy(&stdout_buf).trim().to_string();
            let stderr = String::from_utf8_lossy(&stderr_buf).trim().to_string();

            if status.success() {
                let out = if !stdout.is_empty() {
                    stdout
                } else if !stderr.is_empty() {
                    stderr
                } else {
                    "(Python executed with no output)".to_string()
                };
                (true, out)
            } else {
                let err_msg = if !stderr.is_empty() { stderr } else { stdout };
                (
                    false,
                    format!(
                        "[Python Error, Exit {}]: {}",
                        status.code().unwrap_or(-1),
                        err_msg
                    ),
                )
            }
        }
        Ok(Err(e)) => (false, format!("[Execution Error]: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            (
                false,
                "[Execution Timeout]: Python script exceeded 25s limit.".to_string(),
            )
        }
    }
}

pub async fn run_shell_command(command_str: &str, cwd: Option<&str>) -> (bool, String) {
    let mut clean_cmd = command_str.trim().to_string();
    if clean_cmd.starts_with("```") {
        if let Some(first_nl) = clean_cmd.find('\n') {
            clean_cmd = clean_cmd[first_nl + 1..].to_string();
        }
        if let Some(last_fence) = clean_cmd.rfind("```") {
            clean_cmd = clean_cmd[..last_fence].to_string();
        }
        clean_cmd = clean_cmd.trim().to_string();
    }

    let is_windows = cfg!(target_os = "windows");
    let mut cmd = if is_windows {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(&clean_cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&clean_cmd);
        c
    };

    if let Some(dir) = cwd {
        if Path::new(dir).exists() {
            cmd.current_dir(dir);
        }
    }

    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, format!("[Shell Spawn Error]: {}", e)),
    };

    let wait_res = timeout(Duration::from_secs(45), child.wait()).await;
    match wait_res {
        Ok(Ok(status)) => {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut stdout_buf).await;
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_end(&mut stderr_buf).await;
            }

            let stdout = String::from_utf8_lossy(&stdout_buf).trim().to_string();
            let stderr = String::from_utf8_lossy(&stderr_buf).trim().to_string();

            let mut combined = String::new();
            if !stdout.is_empty() {
                combined.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push_str("\n--- STDERR ---\n");
                }
                combined.push_str(&stderr);
            }

            let char_count = combined.chars().count();
            if char_count > 4000 {
                let truncated: String = combined.chars().take(3800).collect();
                combined = format!("{}...\n[Truncated {} chars]", truncated, char_count - 3800);
            }

            if combined.is_empty() {
                combined = "(Command completed with code 0)".to_string();
            }

            (status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("[Shell Error]: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            (
                false,
                "[Shell Timeout]: Command exceeded 45s limit.".to_string(),
            )
        }
    }
}

use crate::engine::client::strip_residual_tags;

pub async fn run_web_scout_fetch(url: &str) -> (bool, String) {
    let stripped = strip_residual_tags(url);
    let clean_url = stripped.trim().trim_matches('"').trim_matches('\'').trim();
    if let Some(bin) = find_web_scout_bin() {
        let mut cmd = Command::new(bin);
        cmd.arg("fetch").arg(clean_url).arg("5000");
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Ok(mut child) = cmd.spawn() {
            if let Ok(Ok(status)) = timeout(Duration::from_secs(15), child.wait()).await {
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut buf).await;
                }
                let content = String::from_utf8_lossy(&buf).trim().to_string();
                if status.success() && !content.is_empty() {
                    return (true, content);
                }
            }
        }
    }

    // Native Rust HTTP fetch fallback if web_scout binary is not in PATH or local directory
    let client_res = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(10))
        .build();

    if let Ok(client) = client_res {
        if let Ok(resp) = client.get(clean_url).send().await {
            if resp.status().is_success() {
                if let Ok(html) = resp.text().await {
                    let text = strip_html_tags(&html);
                    let lines: Vec<&str> = text
                        .lines()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty())
                        .collect();
                    let cleaned_body = lines.join("\n");
                    let sample: String = cleaned_body.chars().take(5000).collect();
                    if !sample.trim().is_empty() {
                        return (true, sample.trim().to_string());
                    }
                }
            }
        }
    }

    (false, format!("Failed to read URL via web_scout: {}", url))
}

pub fn clean_search_query(raw: &str) -> String {
    let stripped = strip_residual_tags(raw);
    let mut q = stripped.trim().to_string();

    // Strip markdown code fences if wrapped
    if q.starts_with("```") {
        if let Some(first_nl) = q.find('\n') {
            q = q[first_nl + 1..].to_string();
        }
        if let Some(last_fence) = q.rfind("```") {
            q = q[..last_fence].to_string();
        }
        q = q.trim().to_string();
    }

    // Strip quotes
    if ((q.starts_with('"') && q.ends_with('"')) || (q.starts_with('\'') && q.ends_with('\'')))
        && q.len() >= 2 {
            q = q[1..q.len() - 1].trim().to_string();
        }

    // Strip common pseudo prefixes like "query>", "query:", "<query>", "search>", "search:", "q:", "q>"
    let prefixes = [
        "query>", "query:", "<query>", "search>", "search:", "q:", "q>",
    ];
    for p in &prefixes {
        let lower = q.to_lowercase();
        if lower.starts_with(p) {
            q = q[p.len()..].trim().to_string();
            break;
        }
    }

    // Strip trailing pseudo tags
    let suffixes = ["</query>", "</parameter>", "</search>", "```"];
    for s in &suffixes {
        let lower = q.to_lowercase();
        if lower.ends_with(s) {
            q = q[..q.len() - s.len()].trim().to_string();
            break;
        }
    }

    // Strip search operators like site:..., filetype:...
    if let Ok(re_op) = regex::Regex::new(r#"(?i)(?:site|filetype|ext):\S+"#) {
        q = re_op.replace_all(&q, "").trim().to_string();
    }

    q
}

pub fn simplify_query_keywords(raw: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "a", "an", "the", "in", "on", "of", "for", "to", "and", "or", "with", "by", "at", "from",
        "is", "are", "was", "were", "be", "been", "as", "into", "through", "over", "between",
        "about", "how", "what", "why", "when", "where", "which", "who", "whom", "this", "that",
        "these", "those", "can", "could", "should", "would", "do", "does", "did", "not", "no",
        "vs", "versus", "using", "towards"
    ];

    let tokens: Vec<&str> = raw
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':' || c == '"' || c == '\'' || c == '(' || c == ')')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && s.len() >= 2)
        .filter(|s| !STOPWORDS.contains(&s.to_lowercase().as_str()))
        .collect();

    if tokens.is_empty() {
        return raw.to_string();
    }

    tokens.join(" ")
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    // Clean common entities
    out.replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                decoded.push(byte);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

async fn native_ddg_html_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let encoded_q = urlencoding_encode(query);
    let url = format!("https://html.duckduckgo.com/html/?q={}", encoded_q);

    let resp = client
        .get(&url)
        .header("Referer", "https://html.duckduckgo.com/")
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9,ru;q=0.8")
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Body read error: {}", e))?;

    if resp.to_lowercase().contains("anomaly") {
        return Err("DDG anomaly detected".to_string());
    }

    let title_regex = regex::Regex::new(
        r#"(?s)<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .map_err(|e| e.to_string())?;
    let title_regex_alt = regex::Regex::new(
        r#"(?s)<a[^>]+href="([^"]+)"[^>]+class="[^"]*result__a[^"]*"[^>]*>(.*?)</a>"#,
    )
    .map_err(|e| e.to_string())?;
    let snip_regex = regex::Regex::new(
        r#"(?s)<(?:a|div)[^>]+class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</(?:a|div)>"#,
    )
    .map_err(|e| e.to_string())?;

    let parts: Vec<&str> = resp.split("result__body").collect();
    let mut results = Vec::new();

    for block in parts.into_iter().skip(1) {
        let (raw_href, raw_title) = if let Some(c) = title_regex.captures(block) {
            (c.get(1).map(|m| m.as_str()), c.get(2).map(|m| m.as_str()))
        } else if let Some(c) = title_regex_alt.captures(block) {
            (c.get(1).map(|m| m.as_str()), c.get(2).map(|m| m.as_str()))
        } else {
            (None, None)
        };

        if let (Some(href), Some(title_html)) = (raw_href, raw_title) {
            let u = if href.contains("uddg=") {
                let p: Vec<&str> = href.split("uddg=").collect();
                if p.len() > 1 {
                    let end_pos = p[1].find('&').unwrap_or(p[1].len());
                    percent_decode(&p[1][..end_pos])
                } else {
                    href.to_string()
                }
            } else {
                href.to_string()
            };

            let t = strip_html_tags(title_html);
            let snip = snip_regex
                .captures(block)
                .map(|c| strip_html_tags(&c[1]))
                .unwrap_or_default();

            if !u.is_empty() && !t.is_empty() {
                results.push(serde_json::json!({
                    "title": t,
                    "url": u,
                    "snippet": snip
                }));
                if results.len() >= limit {
                    break;
                }
            }
        }
    }

    if results.is_empty() {
        return Err("No search results parsed from DDG HTML".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

async fn native_ddg_lite_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(7))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let params = [("q", query)];
    let resp = client
        .post("https://lite.duckduckgo.com/lite/")
        .form(&params)
        .header("Referer", "https://lite.duckduckgo.com/")
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9,ru;q=0.8")
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Body read error: {}", e))?;

    if resp.to_lowercase().contains("anomaly") {
        return Err("DDG Lite anomaly detected".to_string());
    }

    let link_regex = regex::Regex::new(r#"(?s)<a[^>]+href=['"]([^'"]+)['"][^>]*class=['"][^'"]*result-link[^'"]*['"][^>]*>(.*?)</a>"#)
        .map_err(|e| e.to_string())?;
    let link_regex_alt = regex::Regex::new(r#"(?s)<a[^>]+class=['"][^'"]*result-link[^'"]*['"][^>]+href=['"]([^'"]+)['"][^>]*>(.*?)</a>"#)
        .map_err(|e| e.to_string())?;
    let snip_regex = regex::Regex::new(
        r#"(?s)<td[^>]*class=['"][^'"]*result-snippet[^'"]*['"][^>]*>(.*?)</td>"#,
    )
    .map_err(|e| e.to_string())?;

    let row_regex = regex::Regex::new(r#"(?s)<tr>(.*?)</tr>"#).map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    let mut current_url = String::new();
    let mut current_title = String::new();

    for row_cap in row_regex.captures_iter(&resp) {
        let row_content = &row_cap[1];
        let link_match = link_regex
            .captures(row_content)
            .or_else(|| link_regex_alt.captures(row_content));
        if let Some(c) = link_match {
            let raw_u = &c[1];
            current_url = if raw_u.contains("uddg=") {
                let p: Vec<&str> = raw_u.split("uddg=").collect();
                if p.len() > 1 {
                    let end_pos = p[1].find('&').unwrap_or(p[1].len());
                    percent_decode(&p[1][..end_pos])
                } else {
                    raw_u.to_string()
                }
            } else {
                raw_u.to_string()
            };
            current_title = strip_html_tags(&c[2]);
            continue;
        }

        if let Some(s_cap) = snip_regex.captures(row_content) {
            if !current_url.is_empty() && !current_title.is_empty() {
                let snip = strip_html_tags(&s_cap[1]);
                results.push(serde_json::json!({
                    "title": current_title.clone(),
                    "url": current_url.clone(),
                    "snippet": snip
                }));
                current_url.clear();
                current_title.clear();
                if results.len() >= limit {
                    break;
                }
            }
        }
    }

    if results.is_empty() {
        return Err("No search results in DDG Lite".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

async fn native_ddg_instant_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let encoded_q = urlencoding_encode(query);
    let url = format!("https://api.duckduckgo.com/?q={}&format=json", encoded_q);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Instant search error: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Body read error: {}", e))?;

    let val: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let mut results = Vec::new();

    let abstract_text = val
        .get("AbstractText")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let abstract_url = val
        .get("AbstractURL")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let heading = val.get("Heading").and_then(|v| v.as_str()).unwrap_or(query);

    if !abstract_text.is_empty() && !abstract_url.is_empty() {
        results.push(serde_json::json!({
            "title": heading,
            "url": abstract_url,
            "snippet": abstract_text
        }));
    }

    if let Some(topics) = val.get("RelatedTopics").and_then(|v| v.as_array()) {
        for t in topics {
            let text = t.get("Text").and_then(|v| v.as_str()).unwrap_or("");
            let first_url = t.get("FirstURL").and_then(|v| v.as_str()).unwrap_or("");
            if !text.is_empty() && !first_url.is_empty() {
                results.push(serde_json::json!({
                    "title": text.chars().take(60).collect::<String>(),
                    "url": first_url,
                    "snippet": text
                }));
                if results.len() >= limit {
                    break;
                }
            }
        }
    }

    if results.is_empty() {
        return Err("No instant search results".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

async fn native_wikipedia_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let encoded_query = urlencoding_encode(query);
    // Language detection: if contains Cyrillic characters, query ru.wikipedia.org
    let has_cyrillic = query
        .chars()
        .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
    let domain = if has_cyrillic {
        "ru.wikipedia.org"
    } else {
        "en.wikipedia.org"
    };
    let url = format!(
        "https://{}/w/api.php?action=query&list=search&srsearch={}&format=json&utf8=",
        domain, encoded_query
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Wikipedia request failed: {}", e))?
        .text()
        .await
        .map_err(|e| format!("Wikipedia read error: {}", e))?;

    let val: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let search_items = val
        .get("query")
        .and_then(|q| q.get("search"))
        .and_then(|s| s.as_array())
        .ok_or_else(|| "No search results in Wikipedia response".to_string())?;

    let mut results = Vec::new();
    for item in search_items.iter().take(limit) {
        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let raw_snippet = item.get("snippet").and_then(|s| s.as_str()).unwrap_or("");
        let snippet = strip_html_tags(raw_snippet);
        let article_url = format!("https://{}/wiki/{}", domain, title.replace(' ', "_"));

        if !title.is_empty() {
            results.push(serde_json::json!({
                "title": format!("Wikipedia: {}", title),
                "url": article_url,
                "snippet": snippet
            }));
        }
    }

    if results.is_empty() {
        return Err("Empty Wikipedia results".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

async fn native_openalex_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Silo-ResearchAgent/1.0 (Desktop AI Workbench; mailto:artem@silo.local)")
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let clean_q = clean_search_query(query);
    let encoded_q = urlencoding_encode(&clean_q);
    let url = format!(
        "https://api.openalex.org/works?search={}&per_page={}",
        encoded_q,
        limit.clamp(1, 10)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("OpenAlex request failed: {}", e))?
        .text()
        .await
        .map_err(|e| format!("OpenAlex read error: {}", e))?;

    let val: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let items = val
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "No results array in OpenAlex response".to_string())?;

    let mut results = Vec::new();
    for item in items.iter().take(limit) {
        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
        if title.is_empty() {
            continue;
        }

        let doi = item.get("doi").and_then(|d| d.as_str());
        let landing_url = item
            .get("primary_location")
            .and_then(|p| p.get("landing_page_url"))
            .and_then(|u| u.as_str());
        let openalex_id = item.get("id").and_then(|i| i.as_str()).unwrap_or("");

        let url = doi.or(landing_url).unwrap_or(openalex_id);

        let pub_year = item
            .get("publication_year")
            .and_then(|y| y.as_i64())
            .map(|y| y.to_string())
            .unwrap_or_else(|| "Recent".to_string());

        let mut snippet = String::new();
        if let Some(inv_obj) = item.get("abstract_inverted_index").and_then(|i| i.as_object()) {
            let mut word_positions: Vec<(usize, &str)> = Vec::new();
            for (word, positions) in inv_obj {
                if let Some(pos_arr) = positions.as_array() {
                    for p in pos_arr {
                        if let Some(idx) = p.as_u64() {
                            word_positions.push((idx as usize, word.as_str()));
                        }
                    }
                }
            }
            word_positions.sort_by_key(|&(idx, _)| idx);
            let reconstructed: Vec<&str> = word_positions.into_iter().map(|(_, w)| w).collect();
            snippet = reconstructed.join(" ");
            if snippet.len() > 600 {
                let end = snippet.char_indices().nth(600).map(|(i, _)| i).unwrap_or(snippet.len());
                snippet = format!("{}...", &snippet[..end]);
            }
        }

        if snippet.is_empty() {
            if let Some(topics) = item.get("topics").and_then(|t| t.as_array()) {
                let names: Vec<&str> = topics
                    .iter()
                    .filter_map(|t| t.get("display_name").and_then(|d| d.as_str()))
                    .take(3)
                    .collect();
                if !names.is_empty() {
                    snippet = format!("Topics: {}", names.join(", "));
                }
            }
        }

        results.push(serde_json::json!({
            "title": format!("{} ({})", title, pub_year),
            "url": url,
            "snippet": snippet
        }));
    }

    if results.is_empty() {
        return Err("No OpenAlex papers found".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

async fn native_crossref_search(query: &str, limit: usize) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("Silo-ResearchAgent/1.0 (Desktop AI Workbench; mailto:artem@silo.local)")
        .timeout(Duration::from_secs(7))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let clean_q = clean_search_query(query);
    let encoded_q = urlencoding_encode(&clean_q);
    let url = format!(
        "https://api.crossref.org/works?query={}&rows={}",
        encoded_q,
        limit.clamp(1, 10)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("CrossRef request failed: {}", e))?
        .text()
        .await
        .map_err(|e| format!("CrossRef read error: {}", e))?;

    let val: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let items = val
        .get("message")
        .and_then(|m| m.get("items"))
        .and_then(|i| i.as_array())
        .ok_or_else(|| "No items array in CrossRef response".to_string())?;

    let mut results = Vec::new();
    for item in items.iter().take(limit) {
        let title = item
            .get("title")
            .and_then(|t| t.as_array())
            .and_then(|arr| arr.first())
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if title.is_empty() {
            continue;
        }

        let doi_url = item
            .get("DOI")
            .and_then(|d| d.as_str())
            .map(|doi| format!("https://doi.org/{}", doi))
            .unwrap_or_else(|| {
                item.get("URL")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string()
            });

        let mut snippet = String::new();
        if let Some(abs) = item.get("abstract").and_then(|a| a.as_str()) {
            snippet = strip_html_tags(abs);
            if snippet.len() > 500 {
                let end = snippet.char_indices().nth(500).map(|(i, _)| i).unwrap_or(snippet.len());
                snippet = format!("{}...", &snippet[..end]);
            }
        }

        results.push(serde_json::json!({
            "title": title,
            "url": doi_url,
            "snippet": snippet
        }));
    }

    if results.is_empty() {
        return Err("No CrossRef entries found".to_string());
    }

    serde_json::to_string(&results).map_err(|e| e.to_string())
}

fn urlencoding_encode(input: &str) -> String {
    let mut encoded = String::new();
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            encoded.push(b as char);
        } else if b == b' ' {
            encoded.push('+');
        } else {
            encoded.push_str(&format!("%{:02X}", b));
        }
    }
    encoded
}

pub fn extract_query_arg(args_str: &str) -> String {
    let mut raw_extracted = args_str.to_string();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
        if let Some(obj) = val.as_object() {
            let candidate_keys = [
                "query",
                "q",
                "search_query",
                "search",
                "search_term",
                "keywords",
                "keyword",
                "topic",
                "input",
                "text",
                "prompt",
            ];
            for k in &candidate_keys {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    raw_extracted = v.to_string();
                    break;
                }
            }
            if raw_extracted == args_str {
                for (_k, v) in obj {
                    if let Some(s) = v.as_str() {
                        raw_extracted = s.to_string();
                        break;
                    }
                }
            }
        }
    } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
        if let Some(obj) = val.as_object() {
            let candidate_keys = [
                "query",
                "q",
                "search_query",
                "search",
                "search_term",
                "keywords",
                "keyword",
                "topic",
                "input",
                "text",
                "prompt",
            ];
            for k in &candidate_keys {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    raw_extracted = v.to_string();
                    break;
                }
            }
            if raw_extracted == args_str {
                for (_k, v) in obj {
                    if let Some(s) = v.as_str() {
                        raw_extracted = s.to_string();
                        break;
                    }
                }
            }
        }
    }

    // Strip trailing `, limit=...` or `, max_results=...` if model passed comma-separated kwargs
    let mut cleaned = raw_extracted.trim().to_string();
    if let Ok(re_limit) = regex::Regex::new(r#"(?i),\s*(?:limit|max_results)\s*=\s*\d+"#) {
        cleaned = re_limit.replace_all(&cleaned, "").trim().to_string();
    }
    // Strip leading `query=` or `q=` if present
    if let Ok(re_q) = regex::Regex::new(r#"(?i)^(?:query|q)\s*=\s*"#) {
        cleaned = re_q.replace(&cleaned, "").trim().to_string();
    }
    // Strip wrapping quotes if entire query was quoted
    if ((cleaned.starts_with('"') && cleaned.ends_with('"'))
        || (cleaned.starts_with('\'') && cleaned.ends_with('\'')))
        && cleaned.len() >= 2
    {
        cleaned = cleaned[1..cleaned.len() - 1].trim().to_string();
    }
    cleaned
}

pub fn extract_url_arg(args_str: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
        if let Some(obj) = val.as_object() {
            let candidate_keys = ["url", "link", "target_url", "href", "input"];
            for k in &candidate_keys {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    return v.to_string();
                }
            }
            for (_k, v) in obj {
                if let Some(s) = v.as_str() {
                    if s.starts_with("http://") || s.starts_with("https://") {
                        return s.to_string();
                    }
                }
            }
        }
    } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
        if let Some(obj) = val.as_object() {
            let candidate_keys = ["url", "link", "target_url", "href", "input"];
            for k in &candidate_keys {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    return v.to_string();
                }
            }
        }
    }
    args_str.to_string()
}

pub fn extract_limit_arg(args_str: &str) -> Option<usize> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
        if let Some(obj) = val.as_object() {
            if let Some(l) = obj.get("limit").or_else(|| obj.get("max_results")).and_then(|v| v.as_u64()) {
                return Some(l as usize);
            }
        }
    } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
        if let Some(obj) = val.as_object() {
            if let Some(l) = obj.get("limit").or_else(|| obj.get("max_results")).and_then(|v| v.as_u64()) {
                return Some(l as usize);
            }
        }
    }
    // Regex fallback for loose kwargs: limit=15 or max_results: 15
    if let Ok(re) = regex::Regex::new(r#"(?i)(?:limit|max_results)\s*[:=]\s*(\d+)"#) {
        if let Some(cap) = re.captures(args_str) {
            if let Ok(num) = cap[1].parse::<usize>() {
                return Some(num);
            }
        }
    }
    None
}

async fn fetch_arxiv_xml(client: &reqwest::Client, search_query: &str, max_results: usize) -> Result<String, String> {
    let url = format!(
        "https://export.arxiv.org/api/query?search_query={}&start=0&max_results={}&sortBy=relevance&sortOrder=descending",
        search_query, max_results
    );
    let resp = client.get(&url).send().await.map_err(|e| format!("Failed to reach arXiv API: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("arXiv API returned HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| format!("Failed to read arXiv response body: {}", e))
}

fn parse_arxiv_entries(xml: &str) -> Vec<String> {
    let entry_re = match regex::Regex::new(r"(?s)<entry>(.*?)</entry>") {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let title_re = regex::Regex::new(r"(?s)<title>(.*?)</title>").unwrap();
    let summary_re = regex::Regex::new(r"(?s)<summary>(.*?)</summary>").unwrap();
    let published_re = regex::Regex::new(r"<published>(.*?)</published>").unwrap();
    let name_re = regex::Regex::new(r"<name>(.*?)</name>").unwrap();
    let id_re = regex::Regex::new(r"<id>(.*?)</id>").unwrap();

    let mut papers = Vec::new();

    for cap in entry_re.captures_iter(xml) {
        let entry_str = &cap[1];

        let raw_title = title_re
            .captures(entry_str)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let title: String = raw_title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            continue;
        }

        let raw_summary = summary_re
            .captures(entry_str)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let summary: String = raw_summary.split_whitespace().collect::<Vec<_>>().join(" ");

        let published = published_re
            .captures(entry_str)
            .map(|c| {
                let p = &c[1];
                if p.len() >= 10 {
                    p[..10].to_string()
                } else {
                    p.to_string()
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());

        let authors: Vec<String> = name_re
            .captures_iter(entry_str)
            .map(|c| c[1].trim().to_string())
            .collect();
        let author_str = if authors.is_empty() {
            "Unknown".to_string()
        } else if authors.len() > 3 {
            format!("{}, et al.", authors[..3].join(", "))
        } else {
            authors.join(", ")
        };

        let link = id_re
            .captures(entry_str)
            .map(|c| c[1].trim().to_string())
            .unwrap_or_default();

        let arxiv_id = if let Some(pos) = link.rfind('/') {
            link[pos + 1..].to_string()
        } else {
            link.clone()
        };

        papers.push(format!(
            "### [{}] {}
- **arXiv ID:** `{}` | **Published:** {}
- **Authors:** {}
- **URL:** {}
- **Abstract:**
{}
",
            papers.len() + 1,
            title,
            arxiv_id,
            published,
            author_str,
            link,
            summary
        ));
    }
    papers
}

pub async fn run_arxiv_search(raw_query: &str, limit: usize) -> (bool, String) {
    let query = extract_query_arg(raw_query);
    if query.is_empty() {
        return (false, "arXiv query is empty.".to_string());
    }

    let max_results = limit.clamp(1, 15);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Silo-ResearchAgent/1.0 (Desktop AI Workbench; https://github.com/herabel/silo)")
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("Failed to create HTTP client: {}", e)),
    };

    // 1. Direct pass-through if model provided explicit arXiv operators
    let has_explicit_ops = query.contains(" AND ")
        || query.contains(" OR ")
        || query.contains(" ANDNOT ")
        || query.contains("ti:")
        || query.contains("abs:")
        || query.contains("cat:")
        || query.contains("au:");

    if has_explicit_ops {
        let direct_query = query.replace(' ', "+").replace('"', "%22");
        if let Ok(xml) = fetch_arxiv_xml(&client, &direct_query, max_results).await {
            let papers = parse_arxiv_entries(&xml);
            if !papers.is_empty() {
                let result = format!(
                    "Found {} peer-reviewed/preprint papers on arXiv for '{}':

{}",
                    papers.len(),
                    query,
                    papers.join("
---

")
                );
                return (true, result);
            }
        }
    }

    // 2. Intelligent keyword and phrase processing
    const ARX_STOPWORDS: &[&str] = &[
        "a", "an", "the", "in", "on", "of", "for", "to", "and", "or", "with", "by", "at", "from",
        "is", "are", "was", "were", "be", "been", "as", "into", "through", "over", "between",
        "high", "throughput", "fast", "novel", "new", "recent", "study", "analysis", "approach",
        "framework", "method", "methods", "system", "systems", "survey", "evaluation", "based",
        "using", "towards", "performance", "effective", "efficient", "efficiently", "improved",
        "paper", "papers", "preprint", "arxiv", "distribution", "dual", "comprehensive", "review",
        "algorithm", "algorithms", "optimization", "optimizations", "large", "small"
    ];

    // Check for quoted subphrases e.g. "content-defined chunking" or "prompt injection"
    let mut exact_phrases = Vec::new();
    if let Ok(quote_re) = regex::Regex::new(r#""([^"]+)""#) {
        for cap in quote_re.captures_iter(&query) {
            let p = cap[1].trim();
            if !p.is_empty() {
                exact_phrases.push(format!("%22{}%22", p.replace(' ', "+")));
            }
        }
    }

    let raw_tokens: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty() && w.len() >= 3)
        .collect();

    let mut filtered_tokens: Vec<String> = raw_tokens
        .iter()
        .filter(|w| !ARX_STOPWORDS.contains(&w.to_lowercase().as_str()))
        .cloned()
        .collect();

    if filtered_tokens.is_empty() {
        filtered_tokens = raw_tokens;
    }

    // Sort tokens by length descending (longest = most specific domain terms e.g. "content-defined", "chunking", "deduplication")
    filtered_tokens.sort_by(|a, b| b.len().cmp(&a.len()));

    // Tier 1: Exact quoted phrases if any
    if !exact_phrases.is_empty() {
        let phrase_query = exact_phrases
            .iter()
            .map(|p| format!("all:{}", p))
            .collect::<Vec<_>>()
            .join("+AND+");
        if let Ok(xml) = fetch_arxiv_xml(&client, &phrase_query, max_results).await {
            let papers = parse_arxiv_entries(&xml);
            if !papers.is_empty() {
                let result = format!(
                    "Found {} peer-reviewed/preprint papers on arXiv for '{}':

{}",
                    papers.len(),
                    query,
                    papers.join("
---

")
                );
                return (true, result);
            }
        }
    }

    // Tier 2: Title and Abstract targeted conjunction on top 2 domain terms
    if filtered_tokens.len() >= 2 {
        let t1 = &filtered_tokens[0];
        let t2 = &filtered_tokens[1];
        let tier2_query = format!("(ti:{}+OR+abs:{})+AND+(ti:{}+OR+abs:{})", t1, t1, t2, t2);
        if let Ok(xml) = fetch_arxiv_xml(&client, &tier2_query, max_results).await {
            let papers = parse_arxiv_entries(&xml);
            if !papers.is_empty() {
                let result = format!(
                    "Found {} peer-reviewed/preprint papers on arXiv for '{}':

{}",
                    papers.len(),
                    query,
                    papers.join("
---

")
                );
                return (true, result);
            }
        }
    }

    // Tier 3: Conjunction across top terms in all:
    let effective_terms = if filtered_tokens.len() > 3 {
        &filtered_tokens[..3]
    } else {
        &filtered_tokens[..]
    };

    let tier3_query = effective_terms
        .iter()
        .map(|t| format!("all:{}", t))
        .collect::<Vec<_>>()
        .join("+AND+");

    if let Ok(xml) = fetch_arxiv_xml(&client, &tier3_query, max_results).await {
        let papers = parse_arxiv_entries(&xml);
        if !papers.is_empty() {
            let result = format!(
                "Found {} peer-reviewed/preprint papers on arXiv for '{}':

{}",
                papers.len(),
                query,
                papers.join("
---

")
            );
            return (true, result);
        }
    }

    // Tier 4: Targeted search for the primary anchor in Title or Abstract (NEVER unconstrained OR across all words)
    if let Some(primary) = filtered_tokens.first() {
        let tier4_query = format!("ti:{}+OR+abs:{}", primary, primary);
        if let Ok(xml) = fetch_arxiv_xml(&client, &tier4_query, max_results).await {
            let papers = parse_arxiv_entries(&xml);
            if !papers.is_empty() {
                let result = format!(
                    "Found {} peer-reviewed/preprint papers on arXiv for '{}':

{}",
                    papers.len(),
                    query,
                    papers.join("
---

")
                );
                return (true, result);
            }
        }
    }

    (
        false,
        format!("No relevant arXiv papers found matching query: '{}'", query),
    )
}

pub async fn run_web_search(raw_query: &str, limit: usize) -> (bool, String) {
    let query = clean_search_query(raw_query);
    if query.is_empty() {
        return (false, "Search query is empty.".to_string());
    }

    // 1. Native Rust DuckDuckGo HTML GET Search
    if let Ok(out) = native_ddg_html_search(&query, limit).await {
        if out.starts_with('[') && out.len() > 2 {
            return (true, out);
        }
    }

    // 2. DuckDuckGo Lite Fallback
    if let Ok(out_lite) = native_ddg_lite_search(&query, limit).await {
        if out_lite.starts_with('[') && out_lite.len() > 2 {
            return (true, out_lite);
        }
    }

    // 2b. Retry with keyword-simplified query on DDG
    let simplified = simplify_query_keywords(&query);
    if !simplified.is_empty() && simplified != query {
        if let Ok(out_simp) = native_ddg_html_search(&simplified, limit).await {
            if out_simp.starts_with('[') && out_simp.len() > 2 {
                return (true, out_simp);
            }
        }
        if let Ok(out_lite_simp) = native_ddg_lite_search(&simplified, limit).await {
            if out_lite_simp.starts_with('[') && out_lite_simp.len() > 2 {
                return (true, out_lite_simp);
            }
        }
    }

    // 3. Academic & Technical Literature Fallback (OpenAlex - 250M+ peer-reviewed papers & preprints)
    if let Ok(out_alex) = native_openalex_search(&query, limit).await {
        if out_alex.starts_with('[') && out_alex.len() > 2 {
            return (true, out_alex);
        }
    }
    if !simplified.is_empty() && simplified != query {
        if let Ok(out_alex) = native_openalex_search(&simplified, limit).await {
            if out_alex.starts_with('[') && out_alex.len() > 2 {
                return (true, out_alex);
            }
        }
    }

    // 3b. Try specific technical domain anchor on OpenAlex (e.g. "KyberSlash", "Hertzbleed")
    let q_words: Vec<&str> = query.split_whitespace().collect();
    if q_words.len() > 1 {
        for w in &q_words {
            let clean_w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if clean_w.len() >= 7 && !clean_w.eq_ignore_ascii_case("attack") && !clean_w.eq_ignore_ascii_case("timing") && !clean_w.eq_ignore_ascii_case("vulnerability") {
                if let Ok(out_anchor) = native_openalex_search(clean_w, limit).await {
                    if out_anchor.starts_with('[') && out_anchor.len() > 2 {
                        return (true, out_anchor);
                    }
                }
            }
        }
    }

    // 4. CrossRef Academic Metadata Fallback
    if let Ok(out_cross) = native_crossref_search(&query, limit).await {
        if out_cross.starts_with('[') && out_cross.len() > 2 {
            return (true, out_cross);
        }
    }

    // 5. DuckDuckGo Instant Answer API Fallback
    if let Ok(out_instant) = native_ddg_instant_search(&query, limit).await {
        if out_instant.starts_with('[') && out_instant.len() > 2 {
            return (true, out_instant);
        }
    }

    // 6. Wikipedia Search API Fallback (language-aware ru/en)
    if let Ok(out_wiki) = native_wikipedia_search(&query, limit).await {
        if out_wiki.starts_with('[') && out_wiki.len() > 2 {
            return (true, out_wiki);
        }
    }

    // 7. Fallback to web_scout search if binary is available locally
    if let Some(bin) = find_web_scout_bin() {
        let mut cmd = Command::new(bin);
        cmd.arg("search").arg(&query).arg(limit.to_string());
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Ok(mut child) = cmd.spawn() {
            if let Ok(Ok(status)) = timeout(Duration::from_secs(12), child.wait()).await {
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut buf).await;
                }
                let content = String::from_utf8_lossy(&buf).trim().to_string();
                if status.success() && content.starts_with('[') && content.len() > 2 {
                    return (true, content);
                }
            }
        }
    }

    (false, format!("No search results found for: {}", query))
}

pub async fn run_read_file(
    path_str: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> (bool, String) {
    let stripped = strip_residual_tags(path_str);
    let clean_path = stripped.trim().trim_matches('"').trim_matches('\'').trim();
    if clean_path.is_empty() {
        return (false, "Error: path is required for read_file".to_string());
    }

    let mut resolved_path = PathBuf::from(clean_path);
    if !resolved_path.exists() {
        // 1. Strip current working directory if path is absolute but file wasn't found in CWD
        let mut rel_str = clean_path;
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = Path::new(clean_path).strip_prefix(&cwd) {
                rel_str = rel.to_str().unwrap_or(clean_path);
            }
        }
        let rel_path = Path::new(rel_str.trim_start_matches('\\').trim_start_matches('/'));

        // 2. Search inside skills directories
        let skills_dir = crate::engine::skills::get_skills_dir();
        if skills_dir.exists() {
            let direct_skill_path = skills_dir.join(rel_path);
            if direct_skill_path.exists() && direct_skill_path.is_file() {
                resolved_path = direct_skill_path;
            } else if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.flatten() {
                    let cand = entry.path().join(rel_path);
                    if cand.exists() && cand.is_file() {
                        resolved_path = cand;
                        break;
                    }
                }
            }
        }
    }

    if !resolved_path.exists() {
        return (false, format!("Error: File not found: {}", clean_path));
    }
    if resolved_path.is_dir() {
        return (
            false,
            format!(
                "Error: '{}' is a directory, not a file. Use list_dir instead.",
                clean_path
            ),
        );
    }

    let data = match tokio::fs::read(&resolved_path).await {
        Ok(d) => d,
        Err(e) => {
            return (
                false,
                format!("Failed to read file '{}': {}", clean_path, e),
            )
        }
    };

    // Quick binary check on first 1024 bytes
    let sample = &data[..data.len().min(1024)];
    if sample.contains(&0) {
        return (
            false,
            format!(
                "Cannot display binary file '{}' ({} bytes)",
                clean_path,
                data.len()
            ),
        );
    }

    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len();

    if total_lines == 0 {
        return (
            true,
            format!("File: {} is empty (0 lines, 0 bytes)", clean_path),
        );
    }

    let start = start_line.unwrap_or(1).max(1);
    if start > total_lines {
        return (
            false,
            format!(
                "Error: start_line ({}) exceeds total lines ({}) in {}",
                start, total_lines, clean_path
            ),
        );
    }

    let max_lines_window = 250;
    let end = match end_line {
        Some(e) => e.max(start).min(total_lines),
        None => (start + max_lines_window - 1).min(total_lines),
    };

    let mut out = format!(
        "File: {} (Lines {}-{} of {})\n\n",
        clean_path, start, end, total_lines
    );
    for idx in start..=end {
        out.push_str(&format!("{:4}: {}\n", idx, lines[idx - 1]));
    }

    if end < total_lines {
        out.push_str(&format!(
            "\n[... Remaining {} lines omitted. Use start_line={}, end_line={} to view more ...]\n",
            total_lines - end,
            end + 1,
            (end + max_lines_window).min(total_lines)
        ));
    }

    (true, out)
}

pub async fn run_write_file(path_str: &str, content: &str) -> (bool, String) {
    let stripped = strip_residual_tags(path_str);
    let clean_path = stripped.trim().trim_matches('"').trim_matches('\'').trim();
    if clean_path.is_empty() {
        return (false, "Error: path is required for write_file".to_string());
    }

    let p = Path::new(clean_path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return (
                    false,
                    format!(
                        "Failed to create parent directory '{}': {}",
                        parent.display(),
                        e
                    ),
                );
            }
        }
    }

    match tokio::fs::write(p, content.as_bytes()).await {
        Ok(_) => {
            let line_count = content.lines().count();
            (
                true,
                format!(
                    "Successfully wrote {} bytes ({} lines) to {}",
                    content.len(),
                    line_count,
                    clean_path
                ),
            )
        }
        Err(e) => (
            false,
            format!("Failed to write file '{}': {}", clean_path, e),
        ),
    }
}

pub async fn run_list_dir(path_str: &str, depth: Option<usize>) -> (bool, String) {
    let stripped = strip_residual_tags(path_str);
    let target = stripped.trim().trim_matches('"').trim_matches('\'').trim();
    let root = if target.is_empty() { "." } else { target };
    let p = Path::new(root);
    if !p.exists() {
        return (false, format!("Directory not found: {}", root));
    }
    if !p.is_dir() {
        return (
            false,
            format!(
                "'{}' is not a directory. Use read_file to inspect files.",
                root
            ),
        );
    }

    let max_depth = depth.unwrap_or(2);
    let mut entries = Vec::new();

    fn walk(dir: &Path, current_depth: usize, max_depth: usize, entries: &mut Vec<String>) {
        if current_depth > max_depth {
            return;
        }
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut items: Vec<_> = read.filter_map(|e| e.ok()).collect();
        items.sort_by_key(|e| e.path());

        for item in items {
            let name = item.file_name().to_string_lossy().to_string();
            if name == ".git"
                || name == "target"
                || name == "node_modules"
                || name == "__pycache__"
                || name == ".idea"
                || name == ".vscode"
            {
                continue;
            }
            let path = item.path();
            let is_dir = path.is_dir();
            let indent = "  ".repeat(current_depth - 1);
            if is_dir {
                entries.push(format!("{}[DIR]  {}/", indent, path.display()));
                if current_depth < max_depth {
                    walk(&path, current_depth + 1, max_depth, entries);
                }
            } else {
                let size_str = match item.metadata() {
                    Ok(m) => {
                        let bytes = m.len();
                        if bytes < 1024 {
                            format!("{} B", bytes)
                        } else if bytes < 1024 * 1024 {
                            format!("{:.1} KB", bytes as f64 / 1024.0)
                        } else {
                            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
                        }
                    }
                    Err(_) => "unknown size".to_string(),
                };
                entries.push(format!(
                    "{}[FILE] {} ({})",
                    indent,
                    path.display(),
                    size_str
                ));
            }
            if entries.len() >= 150 {
                entries.push("... (listing truncated at 150 items)".to_string());
                return;
            }
        }
    }

    walk(p, 1, max_depth, &mut entries);
    if entries.is_empty() {
        (true, format!("Directory '{}' is empty.", root))
    } else {
        (
            true,
            format!(
                "Directory listing for '{}' (depth <= {}):\n{}",
                root,
                max_depth,
                entries.join("\n")
            ),
        )
    }
}

pub async fn run_search_files(pattern: &str, path_str: &str) -> (bool, String) {
    let stripped_pat = strip_residual_tags(pattern);
    let clean_pat = stripped_pat
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if clean_pat.is_empty() {
        return (
            false,
            "Error: pattern is required for search_files".to_string(),
        );
    }

    let stripped_path = strip_residual_tags(path_str);
    let target = stripped_path
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    let root = if target.is_empty() { "." } else { target };
    let p = Path::new(root);
    if !p.exists() {
        return (false, format!("Search path not found: {}", root));
    }

    let regex_matcher = regex::RegexBuilder::new(clean_pat)
        .case_insensitive(true)
        .build();

    let mut matches = Vec::new();
    let max_matches = 60;

    fn search_dir(
        dir: &Path,
        matcher: &Result<regex::Regex, regex::Error>,
        literal: &str,
        matches: &mut Vec<String>,
        max_matches: usize,
    ) {
        if matches.len() >= max_matches {
            return;
        }
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in read.filter_map(|e| e.ok()) {
            if matches.len() >= max_matches {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git"
                || name == "target"
                || name == "node_modules"
                || name == "__pycache__"
                || name == ".idea"
                || name == ".vscode"
            {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                search_dir(&path, matcher, literal, matches, max_matches);
            } else if path.is_file() {
                if let Ok(meta) = entry.metadata() {
                    if meta.len() > 5 * 1024 * 1024 {
                        continue;
                    }
                }
                if let Ok(data) = std::fs::read(&path) {
                    if data.len() > 1024 && data[..1024].contains(&0) {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&data);
                    for (line_idx, line) in text.lines().enumerate() {
                        let is_match = match matcher {
                            Ok(re) => re.is_match(line),
                            Err(_) => line.to_lowercase().contains(&literal.to_lowercase()),
                        };
                        if is_match {
                            matches.push(format!(
                                "{}:{}: {}",
                                path.display(),
                                line_idx + 1,
                                line.trim()
                            ));
                            if matches.len() >= max_matches {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    if p.is_file() {
        if let Ok(data) = std::fs::read(p) {
            let text = String::from_utf8_lossy(&data);
            for (line_idx, line) in text.lines().enumerate() {
                let is_match = match &regex_matcher {
                    Ok(re) => re.is_match(line),
                    Err(_) => line.to_lowercase().contains(&clean_pat.to_lowercase()),
                };
                if is_match {
                    matches.push(format!("{}:{}: {}", p.display(), line_idx + 1, line.trim()));
                    if matches.len() >= max_matches {
                        break;
                    }
                }
            }
        }
    } else {
        search_dir(p, &regex_matcher, clean_pat, &mut matches, max_matches);
    }

    if matches.is_empty() {
        (
            true,
            format!("No matches found for '{}' in '{}'", clean_pat, root),
        )
    } else {
        let count = matches.len();
        let suffix = if count >= max_matches {
            "\n... (reached max 60 results, refine your search)"
        } else {
            ""
        };
        (
            true,
            format!(
                "Found {} match(es) for '{}' in '{}':\n{}{}",
                count,
                clean_pat,
                root,
                matches.join("\n"),
                suffix
            ),
        )
    }
}

/// Returns true if the tool does not mutate files or execute arbitrary shell scripts.
/// Read-only tools can safely be executed in parallel.
pub fn is_read_only_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "arxiv_search"
            | "arxiv"
            | "search_arxiv"
            | "web_search"
            | "search"
            | "duckduckgo_search"
            | "google_search"
            | "fetch_page"
            | "fetch"
            | "read_url"
            | "web_fetch"
            | "read_file"
            | "read"
            | "list_dir"
            | "ls"
            | "dir"
            | "search_files"
            | "grep"
            | "search_code"
            | "execute_python"
            | "python"
            | "calc"
            | "eval"
            | "record_finding"
            | "finding"
            | "record_contradiction"
            | "contradiction"
    )
}

pub async fn execute_tool(name: &str, raw_args: &str) -> (bool, String) {
    let lower_name = name.to_lowercase();
    let norm_args = crate::engine::clean_and_normalize_tool_arguments(&lower_name, raw_args);
    let args_str = norm_args.as_str();

    if lower_name == "execute_python"
        || lower_name == "python"
        || lower_name == "calc"
        || lower_name == "eval"
    {
        let code = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            val.get("code")
                .and_then(|c| c.as_str())
                .unwrap_or(args_str)
                .to_string()
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            val.get("code")
                .and_then(|c| c.as_str())
                .unwrap_or(args_str)
                .to_string()
        } else if let Ok(re) = regex::Regex::new(r#"(?s)"code"\s*:\s*"([\s\S]*?)"\s*\}?$"#) {
            if let Some(cap) = re.captures(args_str.trim()) {
                cap[1].to_string()
            } else {
                args_str.to_string()
            }
        } else {
            args_str.to_string()
        };
        run_python_code(&code).await
    } else if lower_name == "run_command"
        || lower_name == "bash"
        || lower_name == "shell"
        || lower_name == "cmd"
    {
        let (cmd, cwd) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let c = val
                .get("cmd")
                .or_else(|| val.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or(args_str)
                .to_string();
            let d = val
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (c, d)
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            let c = val
                .get("cmd")
                .or_else(|| val.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or(args_str)
                .to_string();
            let d = val
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (c, d)
        } else {
            (args_str.to_string(), None)
        };
        run_shell_command(&cmd, cwd.as_deref()).await
    } else if lower_name == "read_file" || lower_name == "read" {
        let (path, start, end) =
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
                let p = val
                    .get("path")
                    .or_else(|| val.get("input"))
                    .or_else(|| val.get("file"))
                    .or_else(|| val.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(args_str)
                    .to_string();
                let s = val
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .map(|u| u as usize);
                let e = val
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .map(|u| u as usize);
                (p, s, e)
            } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
                let p = val
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(args_str)
                    .to_string();
                let s = val
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .map(|u| u as usize);
                let e = val
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .map(|u| u as usize);
                (p, s, e)
            } else {
                (args_str.to_string(), None, None)
            };
        run_read_file(&path, start, end).await
    } else if lower_name == "write_file" || lower_name == "write" {
        let (path, content) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let p = val
                .get("path")
                .or_else(|| val.get("input"))
                .or_else(|| val.get("file"))
                .or_else(|| val.get("filename"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let c = val
                .get("content")
                .or_else(|| val.get("text"))
                .or_else(|| val.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (p, c)
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            let p = val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let c = val
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (p, c)
        } else {
            (args_str.to_string(), String::new())
        };
        run_write_file(&path, &content).await
    } else if lower_name == "list_dir" || lower_name == "ls" || lower_name == "dir" {
        let (path, depth) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let p = val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let d = val
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|u| u as usize);
            (p, d)
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            let p = val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let d = val
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|u| u as usize);
            (p, d)
        } else {
            let clean = args_str.trim().trim_matches('"').trim_matches('\'').trim();
            (
                if clean.is_empty() {
                    ".".to_string()
                } else {
                    clean.to_string()
                },
                None,
            )
        };
        run_list_dir(&path, depth).await
    } else if lower_name == "search_files" || lower_name == "grep" || lower_name == "search_code" {
        let (pattern, path) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let pat = val
                .get("pattern")
                .or_else(|| val.get("query"))
                .or_else(|| val.get("input"))
                .and_then(|v| v.as_str())
                .unwrap_or(args_str)
                .to_string();
            let p = val
                .get("path")
                .or_else(|| val.get("dir"))
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            (pat, p)
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            let pat = val
                .get("pattern")
                .or_else(|| val.get("query"))
                .and_then(|v| v.as_str())
                .unwrap_or(args_str)
                .to_string();
            let p = val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            (pat, p)
        } else {
            (args_str.to_string(), ".".to_string())
        };
        run_search_files(&pattern, &path).await
    } else if lower_name == "web_search"
        || lower_name == "search"
        || lower_name == "duckduckgo_search"
        || lower_name == "google_search"
    {
        let query = extract_query_arg(args_str);
        run_web_search(&query, 4).await
    } else if lower_name == "fetch_page"
        || lower_name == "fetch"
        || lower_name == "read_url"
        || lower_name == "web_fetch"
    {
        let url = extract_url_arg(args_str);
        run_web_scout_fetch(&url).await
    } else if lower_name == "arxiv_search"
        || lower_name == "arxiv"
        || lower_name == "search_arxiv"
    {
        let query = extract_query_arg(args_str);
        let limit = extract_limit_arg(args_str).unwrap_or(4);
        run_arxiv_search(&query, limit).await
    } else if lower_name == "record_finding" || lower_name == "finding" {
        let (source, claim, details, caveats, verified) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let s = val.get("source").or_else(|| val.get("url")).and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
            let c = val.get("claim").or_else(|| val.get("title")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let d = val.get("details").or_else(|| val.get("content")).or_else(|| val.get("notes")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cav = val.get("caveats").or_else(|| val.get("contradiction")).and_then(|v| v.as_str()).map(|s| s.to_string());
            let ver = val.get("verified").and_then(|v| v.as_bool()).unwrap_or(true);
            (s, c, d, cav, ver)
        } else {
            ("Unknown".to_string(), args_str.to_string(), String::new(), None, true)
        };
        (true, crate::engine::record_finding_to_global_board(&source, &claim, &details, caveats.as_deref(), verified))
    } else if lower_name == "record_contradiction" || lower_name == "contradiction" {
        let (point, ca, sa, cb, sb, analysis) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let p = val.get("point").or_else(|| val.get("topic")).and_then(|v| v.as_str()).unwrap_or("Contradiction").to_string();
            let ca = val.get("claim_a").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sa = val.get("source_a").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cb = val.get("claim_b").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sb = val.get("source_b").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let an = val.get("analysis").or_else(|| val.get("resolution")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            (p, ca, sa, cb, sb, an)
        } else {
            ("Conflict".to_string(), args_str.to_string(), "".to_string(), "".to_string(), "".to_string(), "".to_string())
        };
        (true, crate::engine::record_contradiction_to_global_board(&point, &ca, &sa, &cb, &sb, &analysis))
    } else if lower_name == "run_skill" || lower_name == "skill" {
        let (skill, script, args) = if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_str) {
            let sk = val
                .get("skill")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sc = val
                .get("script")
                .or_else(|| val.get("cmd"))
                .or_else(|| val.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let a: Vec<String> = if let Some(arr) = val.get("args").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            } else if let Some(s) = val.get("args").and_then(|v| v.as_str()) {
                crate::engine::client::parse_cli_tokens(s)
            } else {
                Vec::new()
            };
            (sk, sc, a)
        } else if let Some(val) = crate::engine::relaxed_json_parse(args_str) {
            let sk = val
                .get("skill")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sc = val
                .get("script")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (sk, sc, Vec::new())
        } else {
            let mut sk = String::new();
            let mut sc = String::new();
            let mut a = Vec::new();
            for line in args_str.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("skill=") {
                    sk = rest.trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(rest) = trimmed.strip_prefix("script=") {
                    sc = rest.trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(rest) = trimmed.strip_prefix("args=") {
                    let raw_a = rest.trim_matches('"').trim_matches('\'');
                    a = raw_a.split_whitespace().map(|s| s.to_string()).collect();
                }
            }
            if sk.is_empty() {
                let installed = crate::engine::skills::list_installed_skills();
                sk = installed.first().map(|s| s.name.clone()).unwrap_or_default();
            }
            if sc.is_empty() && !args_str.contains('=') {
                sc = args_str.trim().to_string();
            }
            (sk, sc, a)
        };
        if skill.is_empty() || script.is_empty() {
            (false, r#"Usage: run_skill with {"skill": "...", "script": "...", "args": [...]}"#.to_string())
        } else {
            crate::engine::skills::run_skill_script(&skill, &script, &args).await
        }
    } else {
        (false, format!("Unknown tool: {}", name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_native_search() {
        let (ok, res) = run_web_search("Rust programming language", 3).await;
        eprintln!("SEARCH RESULT: ok={}, len={}", ok, res.len());
        assert!(ok, "Search failed: {}", res);
    }

    #[tokio::test]
    async fn test_russian_search() {
        let (ok, res) = run_web_search("новости космонавтики", 3).await;
        eprintln!("RUSSIAN SEARCH RESULT: ok={}, len={}", ok, res.len());
        assert!(ok, "Search failed: {}", res);
    }

    #[tokio::test]
    async fn test_arxiv_search() {
        let (ok, res) = run_arxiv_search(
            "\"dual mask boundary distribution content-defined chunking high throughput\", limit=15",
            3,
        ).await;
        println!("\n=== ARXIV SEARCH RESULT ===\n{}\n===========================\n", res);
        assert!(ok, "arXiv search failed: {}", res);
        assert!(!res.contains("Face Mask"), "Should not match Face Mask");
        assert!(res.contains("Chunk") || res.contains("boundary") || res.contains("content-defined"));
    }

    #[test]
    fn test_is_read_only_tool() {
        assert!(is_read_only_tool("arxiv_search"));
        assert!(is_read_only_tool("web_search"));
        assert!(is_read_only_tool("fetch_page"));
        assert!(is_read_only_tool("read_file"));
        assert!(is_read_only_tool("list_dir"));
        assert!(is_read_only_tool("search_files"));
        assert!(is_read_only_tool("execute_python"));

        assert!(!is_read_only_tool("write_file"));
        assert!(!is_read_only_tool("run_command"));
        assert!(!is_read_only_tool("run_skill"));
    }

    #[test]
    fn test_skill_discovery() {
        let _ = crate::engine::skills::list_installed_skills();
    }
}
