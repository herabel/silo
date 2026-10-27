use crate::engine::stream_parser::{ParsedChunk, StreamParser};
use crate::session::{ChatMessage, MessageRole};
use futures_util::StreamExt;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub is_loaded: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub enum StreamUpdate {
    Token(String),
    Thinking(String),
    ToolDrafting(String),
    Log(String),
    Payload(String),
    Finished {
        full_content: String,
        full_thinking: Option<String>,
        tool_calls: Vec<ToolCallInfo>,
    },
    Error(String),
}

/// Normalizes literal newlines and tabs inside JSON strings (equivalent to Python json.loads(..., strict=False))
pub fn relaxed_json_parse(raw: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        return Some(v);
    }

    let mut normalized = String::with_capacity(raw.len() + 32);
    let mut in_str = false;
    let mut escaped = false;

    for c in raw.chars() {
        if c == '"' && !escaped {
            in_str = !in_str;
            normalized.push(c);
        } else if in_str {
            if c == '\n' {
                normalized.push_str("\\n");
            } else if c == '\r' {
                // skip carriage return
            } else if c == '\t' {
                normalized.push_str("\\t");
            } else {
                if c == '\\' {
                    escaped = !escaped;
                } else {
                    escaped = false;
                }
                normalized.push(c);
            }
        } else {
            normalized.push(c);
        }
    }

    serde_json::from_str::<Value>(&normalized).ok()
}

pub fn parse_fn_call_arguments(fn_name: &str, raw_args: &str) -> String {
    let raw = raw_args.trim();
    if let Some(v) = relaxed_json_parse(raw) {
        if v.is_object() {
            return v.to_string();
        }
    }

    // Match read_file with path, start_line, end_line
    if fn_name == "read_file" || fn_name == "read" {
        let mut path = String::new();
        let mut start_line = None;
        let mut end_line = None;

        if let Ok(p_re) = Regex::new(r#"path\s*[:=]\s*["'](.*?)["']"#) {
            if let Some(cap) = p_re.captures(raw) {
                path = cap[1].to_string();
            }
        }
        if let Ok(s_re) = Regex::new(r#"start_line\s*[:=]\s*(\d+)"#) {
            if let Some(cap) = s_re.captures(raw) {
                start_line = cap[1].parse::<usize>().ok();
            }
        }
        if let Ok(e_re) = Regex::new(r#"end_line\s*[:=]\s*(\d+)"#) {
            if let Some(cap) = e_re.captures(raw) {
                end_line = cap[1].parse::<usize>().ok();
            }
        }

        if !path.is_empty() {
            let mut obj = json!({ "path": path });
            if let Some(s) = start_line {
                obj["start_line"] = json!(s);
            }
            if let Some(e) = end_line {
                obj["end_line"] = json!(e);
            }
            return obj.to_string();
        }
    }

    // Match search_files with pattern and optional path
    if fn_name == "search_files" || fn_name == "grep" || fn_name == "search_code" {
        let mut pattern = String::new();
        let mut path = ".".to_string();
        if let Ok(p_re) = Regex::new(r#"(?:pattern|query)\s*[:=]\s*["'](.*?)["']"#) {
            if let Some(cap) = p_re.captures(raw) {
                pattern = cap[1].to_string();
            }
        }
        if let Ok(p_re) = Regex::new(r#"path\s*[:=]\s*["'](.*?)["']"#) {
            if let Some(cap) = p_re.captures(raw) {
                path = cap[1].to_string();
            }
        }
        if !pattern.is_empty() {
            return json!({ "pattern": pattern, "path": path }).to_string();
        }
    }

    // Match query: "...", query="...", code: "...", cmd: "...", etc.
    if let Ok(re) = Regex::new(r#"(?s)(?:query|url|code|cmd|path|pattern)\s*[:=]\s*["'](.*?)["'](?:\s*,\s*|\s*$)"#) {
        if let Some(cap) = re.captures(raw) {
            let val = cap[1].to_string();
            return match fn_name {
                "web_search" | "search" => json!({ "query": val }).to_string(),
                "fetch_page" | "fetch" => json!({ "url": val }).to_string(),
                "execute_python" | "python" => json!({ "code": val }).to_string(),
                "run_command" | "cmd" => json!({ "cmd": val }).to_string(),
                "read_file" | "read" => json!({ "path": val }).to_string(),
                "list_dir" | "ls" | "dir" => json!({ "path": val }).to_string(),
                "search_files" | "grep" | "search_code" => json!({ "pattern": val }).to_string(),
                _ => json!({ "input": val }).to_string(),
            };
        }
    }

    // Match bare quoted string "..." or '...'
    if let Ok(str_re) = Regex::new(r#"^["'](.*)["']$"#) {
        if let Some(cap) = str_re.captures(raw) {
            let val = cap[1].to_string();
            return match fn_name {
                "web_search" | "search" => json!({ "query": val }).to_string(),
                "fetch_page" | "fetch" => json!({ "url": val }).to_string(),
                "execute_python" | "python" => json!({ "code": val }).to_string(),
                "run_command" | "cmd" => json!({ "cmd": val }).to_string(),
                "read_file" | "read" => json!({ "path": val }).to_string(),
                "list_dir" | "ls" | "dir" => json!({ "path": val }).to_string(),
                "search_files" | "grep" | "search_code" => json!({ "pattern": val }).to_string(),
                _ => json!({ "input": val }).to_string(),
            };
        }
    }

    clean_and_normalize_tool_arguments(fn_name, raw)
}




pub fn strip_residual_tags(s: &str) -> String {
    let t = s.trim();
    let mut cleaned = t.to_string();

    // 1. Strip surrounding XML tags like <tag>...</tag>
    if cleaned.starts_with('<') {
        if let Some(first_gt) = cleaned.find('>') {
            let tag_name = cleaned[1..first_gt].trim();
            let close_tag = format!("</{}>", tag_name);
            let lower = cleaned.to_lowercase();
            let lower_close = close_tag.to_lowercase();
            if lower.ends_with(&lower_close) {
                cleaned = cleaned[first_gt + 1..cleaned.len() - close_tag.len()].trim().to_string();
            }
        }
    }

    // 2. Strip stray opening/closing tags like </url>, <path>, <parameter=url>, <invoke>
    if let Ok(re) = Regex::new(r"</?[a-zA-Z0-9_-]+(?:=[^>\n]*>|=[^>\n]*$|>|$)") {
        cleaned = re.replace_all(&cleaned, "").trim().to_string();
    }

    // 3. Strip key= or key: prefixes (e.g. "path=...", "url=...", "query=...")
    let prefixes = ["path=", "path:", "url=", "url:", "query=", "query:", "q=", "q:", "pattern=", "pattern:", "cmd=", "cmd:", "code=", "code:"];
    for p in &prefixes {
        let lower = cleaned.to_lowercase();
        if lower.starts_with(p) {
            cleaned = cleaned[p.len()..].trim().to_string();
            break;
        }
    }

    // 4. Strip quotes
    if (cleaned.starts_with('"') && cleaned.ends_with('"')) || (cleaned.starts_with('\'') && cleaned.ends_with('\'')) {
        if cleaned.len() >= 2 {
            cleaned = cleaned[1..cleaned.len() - 1].trim().to_string();
        }
    }

    cleaned
}


pub fn clean_and_normalize_tool_arguments(tool_name: &str, raw_args: &str) -> String {
    let raw_trimmed = raw_args.trim();
    if raw_trimmed.is_empty() {
        return "{}".to_string();
    }

    let norm_name = match tool_name.to_lowercase().replace('_', "").as_str() {
        "readfile" | "read" => "read_file",
        "writefile" | "write" => "write_file",
        "searchfiles" | "grep" | "searchcode" => "search_files",
        "listdir" | "ls" | "dir" => "list_dir",
        "executepython" | "python" | "calc" | "eval" => "execute_python",
        "runcommand" | "cmd" | "bash" | "shell" => "run_command",
        "websearch" | "search" | "duckduckgosearch" | "googlesearch" => "web_search",
        "fetchpage" | "fetch" => "fetch_page",
        _ => tool_name,
    };

    // 1. If valid JSON object, check and clean string fields
    if let Ok(mut val) = serde_json::from_str::<Value>(raw_trimmed) {
        if let Some(obj) = val.as_object_mut() {
            if obj.contains_key("input") {
                let inp = obj.remove("input").unwrap();
                match norm_name {
                    "write_file" | "read_file" | "list_dir" => {
                        if !obj.contains_key("path") {
                            obj.insert("path".to_string(), inp);
                        }
                    }
                    "search_files" => {
                        if !obj.contains_key("pattern") {
                            obj.insert("pattern".to_string(), inp);
                        }
                    }
                    "web_search" => {
                        if !obj.contains_key("query") {
                            obj.insert("query".to_string(), inp);
                        }
                    }
                    "fetch_page" => {
                        if !obj.contains_key("url") {
                            obj.insert("url".to_string(), inp);
                        }
                    }
                    "run_command" => {
                        if !obj.contains_key("cmd") {
                            obj.insert("cmd".to_string(), inp);
                        }
                    }
                    "execute_python" => {
                        if !obj.contains_key("code") {
                            obj.insert("code".to_string(), inp);
                        }
                    }
                    _ => {}
                }
            }
            for (k, v) in obj.iter_mut() {
                if let Some(s) = v.as_str() {
                    if k != "code" && k != "content" {
                        let cleaned = strip_residual_tags(s);
                        *v = Value::String(cleaned);
                    }
                }
            }
            if norm_name == "write_file" && !obj.contains_key("content") {
                obj.insert("content".to_string(), Value::String(String::new()));
            }
            if norm_name == "search_files" && !obj.contains_key("path") {
                obj.insert("path".to_string(), Value::String(".".to_string()));
            }
            return serde_json::to_string(&val).unwrap_or_else(|_| "{}".to_string());
        }
    }

    // 2. If relaxed JSON parse succeeds
    if let Some(mut val) = relaxed_json_parse(raw_trimmed) {
        if let Some(obj) = val.as_object_mut() {
            if obj.contains_key("input") {
                let inp = obj.remove("input").unwrap();
                match norm_name {
                    "write_file" | "read_file" | "list_dir" => {
                        if !obj.contains_key("path") {
                            obj.insert("path".to_string(), inp);
                        }
                    }
                    "search_files" => {
                        if !obj.contains_key("pattern") {
                            obj.insert("pattern".to_string(), inp);
                        }
                    }
                    "web_search" => {
                        if !obj.contains_key("query") {
                            obj.insert("query".to_string(), inp);
                        }
                    }
                    "fetch_page" => {
                        if !obj.contains_key("url") {
                            obj.insert("url".to_string(), inp);
                        }
                    }
                    "run_command" => {
                        if !obj.contains_key("cmd") {
                            obj.insert("cmd".to_string(), inp);
                        }
                    }
                    "execute_python" => {
                        if !obj.contains_key("code") {
                            obj.insert("code".to_string(), inp);
                        }
                    }
                    _ => {}
                }
            }
            for (k, v) in obj.iter_mut() {
                if let Some(s) = v.as_str() {
                    if k != "code" && k != "content" {
                        let cleaned = strip_residual_tags(s);
                        *v = Value::String(cleaned);
                    }
                }
            }
            if norm_name == "write_file" && !obj.contains_key("content") {
                obj.insert("content".to_string(), Value::String(String::new()));
            }
            if norm_name == "search_files" && !obj.contains_key("path") {
                obj.insert("path".to_string(), Value::String(".".to_string()));
            }
            return serde_json::to_string(&val).unwrap_or_else(|_| "{}".to_string());
        }
    }

    // 3. Peel off outer <tool_name> ... </tool_name> if present
    let mut clean_input = raw_trimmed;
    let outer_tag = format!("<{}>", norm_name);
    let outer_close = format!("</{}>", norm_name);
    let lower_input = clean_input.to_lowercase();
    if lower_input.starts_with(&outer_tag) {
        clean_input = &clean_input[outer_tag.len()..];
        if clean_input.to_lowercase().ends_with(&outer_close) {
            clean_input = &clean_input[..clean_input.len() - outer_close.len()];
        }
    }
    let clean_input = clean_input.trim();

    // 4. Parse known XML parameter tags without backreferences: <path>...</path>, <content>...</content>
    let known_tags = ["path", "content", "pattern", "query", "q", "url", "code", "cmd", "command", "start_line", "end_line", "depth"];
    let lower_input_for_xml = clean_input.to_lowercase();
    let mut args_map = serde_json::Map::new();
    let mut found_xml_params = false;

    for &tag in &known_tags {
        let open_tag = format!("<{}>", tag);
        let close_tag = format!("</{}>", tag);
        if let Some(start_idx) = lower_input_for_xml.find(&open_tag) {
            let val_start = start_idx + open_tag.len();
            let val_end = if let Some(close_idx) = lower_input_for_xml[val_start..].find(&close_tag) {
                val_start + close_idx
            } else {
                clean_input.len()
            };
            let raw_val = clean_input[val_start..val_end].trim();
            let mut val = raw_val.to_string();
            if tag != "code" && tag != "content" {
                val = strip_residual_tags(&val);
            }
            found_xml_params = true;
            if tag == "start_line" || tag == "end_line" || tag == "depth" {
                if let Ok(num) = val.parse::<i64>() {
                    args_map.insert(tag.to_string(), json!(num));
                    continue;
                }
            }
            args_map.insert(tag.to_string(), Value::String(val));
        }
    }

    if found_xml_params && !args_map.is_empty() {
        if norm_name == "write_file" && !args_map.contains_key("content") {
            args_map.insert("content".to_string(), Value::String(String::new()));
        }
        if norm_name == "search_files" && !args_map.contains_key("path") {
            args_map.insert("path".to_string(), Value::String(".".to_string()));
        }
        return serde_json::to_string(&Value::Object(args_map)).unwrap_or_else(|_| "{}".to_string());
    }

    // 5. Key-Value parameter lines: key=val or key: val (e.g. path=...\ncontent=...)
    let known_keys_pattern = r#"(?mi)^(path|content|pattern|query|q|url|code|cmd|command|start_line|end_line|depth|input)\s*[:=]\s*(.*)$"#;
    if let Ok(kv_re) = Regex::new(known_keys_pattern) {
        let matches: Vec<_> = kv_re.captures_iter(clean_input).collect();
        if !matches.is_empty() {
            let mut kv_map = serde_json::Map::new();
            for (i, cap) in matches.iter().enumerate() {
                let key = cap[1].to_lowercase();
                let val_match = cap.get(2).unwrap();
                let start_idx = val_match.start();
                let end_idx = if i + 1 < matches.len() {
                    matches[i + 1].get(0).unwrap().start()
                } else {
                    clean_input.len()
                };
                let raw_val = clean_input[start_idx..end_idx].trim();
                let mut clean_val = raw_val.to_string();
                if (clean_val.starts_with('"') && clean_val.ends_with('"')) || (clean_val.starts_with('\'') && clean_val.ends_with('\'')) {
                    if clean_val.len() >= 2 {
                        clean_val = clean_val[1..clean_val.len() - 1].to_string();
                        clean_val = clean_val.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"");
                    }
                }
                if key != "code" && key != "content" {
                    clean_val = strip_residual_tags(&clean_val);
                }

                if key == "start_line" || key == "end_line" || key == "depth" {
                    if let Ok(num) = clean_val.trim().parse::<i64>() {
                        kv_map.insert(key, json!(num));
                        continue;
                    }
                }
                kv_map.insert(key, Value::String(clean_val));
            }
            if !kv_map.is_empty() {
                if kv_map.contains_key("input") {
                    let inp = kv_map.remove("input").unwrap();
                    match norm_name {
                        "write_file" | "read_file" | "list_dir" => {
                            if !kv_map.contains_key("path") {
                                kv_map.insert("path".to_string(), inp);
                            }
                        }
                        "search_files" => {
                            if !kv_map.contains_key("pattern") {
                                kv_map.insert("pattern".to_string(), inp);
                            }
                        }
                        "web_search" => {
                            if !kv_map.contains_key("query") {
                                kv_map.insert("query".to_string(), inp);
                            }
                        }
                        "fetch_page" => {
                            if !kv_map.contains_key("url") {
                                kv_map.insert("url".to_string(), inp);
                            }
                        }
                        _ => {}
                    }
                }
                if norm_name == "write_file" && !kv_map.contains_key("content") {
                    kv_map.insert("content".to_string(), Value::String(String::new()));
                }
                if norm_name == "search_files" && !kv_map.contains_key("path") {
                    kv_map.insert("path".to_string(), Value::String(".".to_string()));
                }
                return serde_json::to_string(&Value::Object(kv_map)).unwrap_or_else(|_| "{}".to_string());
            }
        }
    }

    // 6. Fallback: single raw string argument without tags
    let clean_str = strip_residual_tags(clean_input);
    let mut fallback_map = serde_json::Map::new();
    match norm_name {
        "read_file" | "list_dir" => {
            fallback_map.insert("path".to_string(), Value::String(clean_str));
        }
        "write_file" => {
            fallback_map.insert("path".to_string(), Value::String(clean_str));
            fallback_map.insert("content".to_string(), Value::String(String::new()));
        }
        "search_files" => {
            fallback_map.insert("pattern".to_string(), Value::String(clean_str));
            fallback_map.insert("path".to_string(), Value::String(".".to_string()));
        }
        "web_search" => {
            fallback_map.insert("query".to_string(), Value::String(clean_str));
        }
        "fetch_page" => {
            fallback_map.insert("url".to_string(), Value::String(clean_str));
        }
        "run_command" => {
            fallback_map.insert("cmd".to_string(), Value::String(clean_str));
        }
        "execute_python" => {
            fallback_map.insert("code".to_string(), Value::String(clean_input.to_string()));
        }
        _ => {
            fallback_map.insert("input".to_string(), Value::String(clean_str));
        }
    }

    serde_json::to_string(&Value::Object(fallback_map)).unwrap_or_else(|_| "{}".to_string())
}



pub fn extract_text_tool_calls(text: &str) -> Vec<ToolCallInfo> {
    let mut calls = Vec::new();

    // Format 00: Direct tool tags, e.g. <search_files>...</search_files>, <write_file>...</write_file>, etc.
    let direct_tools = [
        "read_file", "readfile", "write_file", "writefile",
        "search_files", "searchfiles", "list_dir", "listdir",
        "execute_python", "python", "run_command", "cmd", "bash",
        "web_search", "websearch", "fetch_page", "fetchpage"
    ];
    let lower_text = text.to_lowercase();
    for &tool_tag in &direct_tools {
        let open_tag = format!("<{}>", tool_tag);
        let close_tag = format!("</{}>", tool_tag);
        let mut cursor = 0;
        while let Some(start_pos) = lower_text[cursor..].find(&open_tag) {
            let abs_start = cursor + start_pos + open_tag.len();
            let (abs_end, next_cursor) = if let Some(end_pos) = lower_text[abs_start..].find(&close_tag) {
                (abs_start + end_pos, abs_start + end_pos + close_tag.len())
            } else {
                (text.len(), text.len())
            };
            let raw_body = text[abs_start..abs_end].trim();
            cursor = next_cursor;

            let norm_name = match tool_tag.replace('_', "").as_str() {
                "readfile" => "read_file",
                "writefile" => "write_file",
                "searchfiles" => "search_files",
                "listdir" => "list_dir",
                "executepython" | "python" => "execute_python",
                "runcommand" | "cmd" | "bash" => "run_command",
                "websearch" => "web_search",
                "fetchpage" => "fetch_page",
                _ => tool_tag,
            }.to_string();

            let norm_args = clean_and_normalize_tool_arguments(&norm_name, raw_body);
            calls.push(ToolCallInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: norm_name,
                arguments: norm_args,
            });
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format 0: Function call with parens e.g. <function=web_search(query: "...") or web_search(query="...") inside <tool_call>
    if let Ok(fn_call_re) = Regex::new(r#"(?s)(?:<function=|\b)(web_search|search|fetch_page|fetch|execute_python|python|run_command|cmd|read_file|read|write_file|write|list_dir|ls|dir|search_files|grep|search_code)\s*\((.*?)\)(?:>|\s*</parameter>|\s*</function>|\s*</tool_call>|$)"#) {
        for cap in fn_call_re.captures_iter(text) {
            let fn_name = cap[1].to_string();
            let raw_args = cap[2].trim();
            let norm_name = match fn_name.as_str() {
                "search" => "web_search",
                "fetch" => "fetch_page",
                "python" => "execute_python",
                "cmd" => "run_command",
                "read" => "read_file",
                "write" => "write_file",
                "ls" | "dir" => "list_dir",
                "grep" | "search_code" => "search_files",
                other => other,
            }.to_string();
            let args_json = parse_fn_call_arguments(&norm_name, raw_args);
            calls.push(ToolCallInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: norm_name,
                arguments: args_json,
            });
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format A: <invoke name="..."> <parameter name="..."> ... </invoke> (Anthropic / Qwen format)
    if let Ok(inv_re) = Regex::new(r#"(?s)<invoke\s+name=["']?([a-zA-Z0-9_]+)["']?>(.*?)(?:</invoke>|$)"#) {
        for cap in inv_re.captures_iter(text) {
            let fn_name = cap[1].to_string();
            let raw_body = cap[2].trim();
            let mut args = json!({});

            if let Ok(param_re) = Regex::new(r#"(?s)<parameter\s+name=["']?([a-zA-Z0-9_]+)["']?>(.*?)(?:</parameter>|$)"#) {
                let mut found_params = false;
                for pcap in param_re.captures_iter(raw_body) {
                    found_params = true;
                    let pname = &pcap[1];
                    let raw_val = pcap[2].trim();
                    let mut clean_val = raw_val.to_string();
                    if let Some(pos) = clean_val.find("</parameter>") {
                        clean_val = clean_val[..pos].to_string();
                    }
                    clean_val = clean_val.trim().to_string();
                    if clean_val.to_lowercase().starts_with("query:\n") || clean_val.to_lowercase().starts_with("query: ") {
                        clean_val = clean_val[6..].trim().to_string();
                    } else if clean_val.to_lowercase().starts_with("url:\n") || clean_val.to_lowercase().starts_with("url: ") {
                        clean_val = clean_val[4..].trim().to_string();
                    }
                    if let Some(inner) = relaxed_json_parse(&clean_val) {
                        if let Some(inner_q) = inner.get("query").or_else(|| inner.get("q")).or_else(|| inner.get("code")).or_else(|| inner.get("cmd")).or_else(|| inner.get("url")) {
                            if let Some(s) = inner_q.as_str() {
                                clean_val = s.to_string();
                            }
                        }
                    }
                    args[pname] = Value::String(clean_val);
                }

                if !found_params {
                    let mut body_str = raw_body.to_string();
                    if let Some(pos) = body_str.find("</invoke>") {
                        body_str = body_str[..pos].to_string();
                    }
                    body_str = body_str.trim().to_string();

                    if let Some(v) = relaxed_json_parse(&body_str) {
                        args = v;
                    } else if fn_name == "execute_python" || fn_name == "python" {
                        args["code"] = Value::String(body_str);
                    } else if fn_name == "run_command" {
                        args["cmd"] = Value::String(body_str);
                    } else if fn_name == "web_search" {
                        args["query"] = Value::String(body_str);
                    } else if fn_name == "fetch_page" {
                        args["url"] = Value::String(body_str);
                    }
                }
            }

            calls.push(ToolCallInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: fn_name,
                arguments: args.to_string(),
            });
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format B: <function=name> ... </function> (or nested inside <tool_call>)
    if let Ok(fn_re) = Regex::new(r#"(?s)<function=([a-zA-Z0-9_]+)[^>]*>(.*?)(?:</function>|$)"#) {
        for cap in fn_re.captures_iter(text) {
            let name = cap[1].to_string();
            let mut raw_args = cap[2].trim().to_string();
            if let Some(pos) = raw_args.find("</parameter>") {
                raw_args = raw_args[..pos].trim().to_string();
            }
            if let Some(pos) = raw_args.find("</tool_call>") {
                raw_args = raw_args[..pos].trim().to_string();
            }

            let args_json = if let Some(v) = relaxed_json_parse(&raw_args) {
                v.to_string()
            } else if name == "execute_python" || name == "python" {
                json!({ "code": raw_args }).to_string()
            } else if name == "run_command" {
                json!({ "cmd": raw_args }).to_string()
            } else if name == "web_search" {
                json!({ "query": raw_args }).to_string()
            } else if name == "fetch_page" {
                json!({ "url": raw_args }).to_string()
            } else {
                raw_args
            };
            calls.push(ToolCallInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name,
                arguments: args_json,
            });
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format C: Standard <tool_call> ... </tool_call>
    if let Ok(tc_re) = Regex::new(r"(?s)<tool_call>(.*?)(?:</tool_call>|$)") {
        for cap in tc_re.captures_iter(text) {
            let raw = cap[1].trim();
            if let Some(val) = relaxed_json_parse(raw) {
                let name = val.get("name").and_then(|n| n.as_str()).unwrap_or("execute_python").to_string();
                let args_str = if let Some(a) = val.get("arguments") {
                    if a.is_string() {
                        a.as_str().unwrap().to_string()
                    } else {
                        a.to_string()
                    }
                } else {
                    raw.to_string()
                };
                calls.push(ToolCallInfo {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    arguments: args_str,
                });
            } else if raw.contains("execute_python") || raw.contains("code") {
                if let Ok(re_code) = Regex::new(r#"(?s)"code"\s*:\s*"([\s\S]*?)"\s*\}?"#) {
                    if let Some(c_cap) = re_code.captures(raw) {
                        calls.push(ToolCallInfo {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: "execute_python".to_string(),
                            arguments: json!({ "code": &c_cap[1] }).to_string(),
                        });
                    }
                }
            }
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format D: Markdown code blocks with JSON tool calls
    if let Ok(block_re) = Regex::new(r#"(?s)```(?:json|tool_call|tool)?\s*(\{\s*"name"\s*:\s*".*?\})\s*```"#) {
        for cap in block_re.captures_iter(text) {
            let raw = cap[1].trim();
            if let Some(val) = relaxed_json_parse(raw) {
                if let Some(name) = val.get("name").and_then(|n| n.as_str()) {
                    let args_str = if let Some(a) = val.get("arguments") {
                        if a.is_string() {
                            a.as_str().unwrap().to_string()
                        } else {
                            a.to_string()
                        }
                    } else {
                        "{}".to_string()
                    };
                    calls.push(ToolCallInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.to_string(),
                        arguments: args_str,
                    });
                }
            }
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format E: Implicit URL fetch detection ONLY if text is a brief single-line command (< 300 chars)
    // NEVER parse markdown citation links in a full assistant response into tool calls!
    let trimmed = text.trim();
    if trimmed.len() < 300 && !trimmed.contains("##") && !trimmed.contains("|---|") {
        let lower = text.to_lowercase();
        if (lower.contains("http://") || lower.contains("https://"))
            && (lower.contains("fetch") || lower.contains("pull"))
        {
            if let Ok(url_re) = Regex::new(r#"https?://[^\s<>"'\)]+"#) {
                for u in url_re.find_iter(text).take(2) {
                    calls.push(ToolCallInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: "fetch_page".to_string(),
                        arguments: json!({ "url": u.as_str() }).to_string(),
                    });
                }
            }
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    // Format F: Extract bare markdown code blocks ONLY if the model emitted exclusively a code block without text explanation
    if calls.is_empty() {
        let trimmed_text = text.trim();
        let is_bare_code = trimmed_text.starts_with("```") && trimmed_text.ends_with("```") && trimmed_text.len() < 2500;
        if is_bare_code {
            if let Ok(py_re) = Regex::new(r#"(?s)^```(?:python|py)\s*\n(.*?)```$"#) {
                if let Some(cap) = py_re.captures(trimmed_text) {
                    let code_body = cap[1].trim();
                    if !code_body.is_empty() {
                        calls.push(ToolCallInfo {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: "execute_python".to_string(),
                            arguments: json!({ "code": code_body }).to_string(),
                        });
                    }
                }
            } else if let Ok(sh_re) = Regex::new(r#"(?s)^```(?:bash|sh|powershell|cmd)\s*\n(.*?)```$"#) {
                if let Some(cap) = sh_re.captures(trimmed_text) {
                    let cmd_body = cap[1].trim();
                    if !cmd_body.is_empty() {
                        calls.push(ToolCallInfo {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: "run_command".to_string(),
                            arguments: json!({ "cmd": cmd_body }).to_string(),
                        });
                    }
                }
            }
        }
    }

    calls
}

pub fn strip_tool_tags(text: &str) -> String {
    let tag_names = [
        "read_file", "readfile", "write_file", "writefile", "search_files", "searchfiles",
        "list_dir", "listdir", "execute_python", "python", "run_command", "cmd", "bash",
        "web_search", "websearch", "fetch_page", "fetchpage", "tool_call", "function",
        "invoke", "parameter", "function_call", "arguments", "think"
    ];
    let mut cleaned = text.to_string();

    for &tag in &tag_names {
        let open_tag_prefix = format!("<{}", tag);
        let close_tag = format!("</{}>", tag);
        let mut loop_count = 0;
        while loop_count < 20 {
            loop_count += 1;
            let lower = cleaned.to_lowercase();
            if let Some(open_idx) = lower.find(&open_tag_prefix) {
                if let Some(tag_end) = lower[open_idx..].find('>') {
                    let body_start = open_idx + tag_end + 1;
                    if let Some(close_idx) = lower[body_start..].find(&close_tag) {
                        let total_end = body_start + close_idx + close_tag.len();
                        cleaned.replace_range(open_idx..total_end, "");
                    } else {
                        cleaned.truncate(open_idx);
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    let orphan_re_str = format!(r"(?si)</?(?:{})[^>]*>", tag_names.join("|"));
    if let Ok(re) = Regex::new(&orphan_re_str) {
        cleaned = re.replace_all(&cleaned, "").to_string();
    }
    cleaned.trim().to_string()
}

pub async fn fetch_available_models(base_url: &str, api_key: Option<&str>) -> Vec<ModelInfo> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .unwrap_or_default();

    let clean_base = base_url.trim().trim_end_matches('/');
    let mut models = Vec::new();

    // 1. Try native api/v0/models if not ending in /v1 (LM Studio convention)
    if !clean_base.ends_with("/v1") {
        let mut req = client.get(format!("{}/api/v0/models", clean_base));
        if let Some(key) = api_key {
            if !key.trim().is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key.trim()));
            }
        }
        if let Ok(resp) = req.send().await {
            if let Ok(val) = resp.json::<Value>().await {
                if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
                    for item in arr {
                        if let Some(id) = item.get("id").and_then(|s| s.as_str()) {
                            let state = item.get("state").and_then(|s| s.as_str()).unwrap_or("");
                            let is_loaded = state == "loaded";
                            models.push(ModelInfo {
                                id: id.to_string(),
                                is_loaded,
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Standard OpenAI /v1/models (llama.cpp, Ollama, vLLM, OpenRouter, Groq, etc.)
    if models.is_empty() {
        let v1_url = if clean_base.ends_with("/v1") {
            format!("{}/models", clean_base)
        } else {
            format!("{}/v1/models", clean_base)
        };
        let mut req = client.get(&v1_url);
        if let Some(key) = api_key {
            if !key.trim().is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key.trim()));
            }
        }
        if let Ok(resp) = req.send().await {
            if let Ok(val) = resp.json::<Value>().await {
                if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
                    for item in arr {
                        if let Some(id) = item.get("id").and_then(|s| s.as_str()) {
                            models.push(ModelInfo {
                                id: id.to_string(),
                                is_loaded: true,
                            });
                        }
                    }
                }
            }
        }
    }

    models
}

pub fn build_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "execute_python",
                "description": "Executes Python code in an isolated subprocess. ALWAYS invoke this tool whenever the user asks for math calculations, simulations, verification, algorithms, or running Python code. Never output raw code into chat without executing this tool first.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": {"type": "string", "description": "The exact Python script or statements to execute."}
                    },
                    "required": ["code"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Executes a shell command (e.g. 'cargo check', 'cargo test', 'git status', 'python') on the host system. Returns stdout, stderr, and exit code. ALWAYS invoke this tool whenever the user asks to run a command or inspect host state.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cmd": {"type": "string", "description": "The exact shell command to execute."},
                        "cwd": {"type": "string", "description": "Optional working directory path"}
                    },
                    "required": ["cmd"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Searches the web for relevant documentation, papers, or articles.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query terms"}
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "fetch_page",
                "description": "Reads full webpage Markdown content using native web_scout.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Full URL to fetch"}
                    },
                    "required": ["url"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Reads file contents with 1-based line numbering (e.g. '123: line content'). To inspect specific functions or code sections without loading huge files, specify 'start_line' and 'end_line' (e.g. start_line: 275, end_line: 315). Always use search_files first if you need to locate where a function or symbol is defined.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute or relative file path to read."},
                        "start_line": {"type": "integer", "description": "1-based starting line number (optional, default 1)."},
                        "end_line": {"type": "integer", "description": "1-based ending line number (optional, default reads up to 250 lines)."}
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Writes or overwrites a file with the given content. Creates parent directories automatically if they do not exist.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute or relative file path to write."},
                        "content": {"type": "string", "description": "The exact full content to write into the file."}
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "Lists contents of a directory (subdirectories and files with sizes). Skips noisy build/vcs folders (.git, target, node_modules).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path (default: '.')"},
                        "depth": {"type": "integer", "description": "Recursion depth (default: 2)"}
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "search_files",
                "description": "Recursively searches for a text pattern or regex across files in a directory. Returns matching file paths and exact line numbers (e.g. 'pkg/auction/engine.go:42: func Run(...)'). Use this to find symbols or definitions before calling read_file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Text substring or regex pattern to search for."},
                        "path": {"type": "string", "description": "Directory or file path to search in (default: '.')"}
                    },
                    "required": ["pattern"]
                }
            }
        })
    ]
}

pub async fn stream_completion(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    enable_tools: bool,
    forced_tool: Option<String>,
    tx: UnboundedSender<StreamUpdate>,
    cancel_token: CancellationToken,
) {
    let client = Client::new();
    let clean_base = base_url.trim().trim_end_matches('/');
    let url = if clean_base.ends_with("/v1") {
        format!("{}/chat/completions", clean_base)
    } else {
        format!("{}/v1/chat/completions", clean_base)
    };

    let mut payload_messages = Vec::new();
    let sys_trimmed = system_prompt.trim();

    // Find the index of the first User message in messages
    let first_user_idx = messages.iter().position(|m| m.role == MessageRole::User);

    // If there are NO user messages at all, but there is a system prompt, send it as user role
    if first_user_idx.is_none() && !sys_trimmed.is_empty() {
        payload_messages.push(json!({
            "role": "user",
            "content": sys_trimmed
        }));
    }

    // Find index of the last assistant message that has substantive text content
    let last_text_assistant_idx = messages.iter().rposition(|m| {
        m.role == MessageRole::Assistant && !m.content.trim().is_empty() && !m.content.trim().starts_with("⚠️")
    });

    for (idx, m) in messages.iter().enumerate() {
        match m.role {
            MessageRole::User => {
                let is_first_user = Some(idx) == first_user_idx;
                let effective_text = if is_first_user && !sys_trimmed.is_empty() {
                    if m.content.trim().is_empty() {
                        sys_trimmed.to_string()
                    } else {
                        format!("{}\n\n---\n\n{}", sys_trimmed, m.content.trim())
                    }
                } else {
                    m.content.trim().to_string()
                };

                if m.images.is_empty() {
                    payload_messages.push(json!({
                        "role": "user",
                        "content": effective_text
                    }));
                } else {
                    let mut parts = Vec::new();
                    let prompt_text = if effective_text.is_empty() {
                        "Please analyze the attached image.".to_string()
                    } else {
                        effective_text
                    };
                    parts.push(json!({
                        "type": "text",
                        "text": prompt_text
                    }));
                    for img in &m.images {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": img.data_url
                            }
                        }));
                    }
                    payload_messages.push(json!({
                        "role": "user",
                        "content": parts
                    }));
                }
            }
            MessageRole::Assistant => {
                let trimmed = m.content.trim();
                // Skip error alert bubbles or placeholder failure notes from previous turns
                if trimmed.starts_with("⚠️")
                    || trimmed.starts_with("*(Model returned an empty response")
                    || trimmed.starts_with("*(Model reasoned internally")
                {
                    continue;
                }

                let is_superseded = last_text_assistant_idx.map_or(false, |last_idx| idx < last_idx);

                if is_superseded {
                    if !trimmed.is_empty() {
                        payload_messages.push(json!({
                            "role": "assistant",
                            "content": trimmed
                        }));
                    }
                } else if !m.tool_calls.is_empty() {
                    let mut tc_objs = Vec::new();
                    for tc in &m.tool_calls {
                        let safe_args = clean_and_normalize_tool_arguments(&tc.name, &tc.arguments);
                        tc_objs.push(json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": safe_args
                            }
                        }));
                    }
                    let asst_obj = json!({
                        "role": "assistant",
                        "content": if trimmed.is_empty() { Value::Null } else { Value::String(trimmed.to_string()) },
                        "tool_calls": tc_objs
                    });
                    // DO NOT attach reasoning_content to past turns: it blows up context window and causes immediate EOS!
                    payload_messages.push(asst_obj);
                } else if !trimmed.is_empty() {
                    payload_messages.push(json!({
                        "role": "assistant",
                        "content": trimmed
                    }));
                }
            }
            MessageRole::System => {
                let trimmed = m.content.trim();
                if !trimmed.is_empty() && true {
                    payload_messages.push(json!({
                        "role": "user",
                        "content": trimmed
                    }));
                }
            }
            MessageRole::Tool => {
                let is_superseded = last_text_assistant_idx.map_or(false, |last_idx| idx < last_idx);
                if !is_superseded {
                    let call_id = m.tool_call_id.clone().unwrap_or_else(|| "call_default".to_string());
                    // Context safety: truncate oversized tool output (e.g. huge HTML dumps) so prompt doesn't exhaust model n_ctx
                    let safe_content = if m.content.chars().count() > 3500 {
                        format!("{}...\n[Tool output truncated for context conservation]", m.content.chars().take(3200).collect::<String>())
                    } else {
                        m.content.clone()
                    };
                    payload_messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": safe_content
                    }));
                }
            }
        }
    }

    // Normalize payload_messages: merge any consecutive user messages to avoid confusing Jinja chat templates
    let mut normalized_messages: Vec<Value> = Vec::new();
    for msg in payload_messages {
        let is_user = msg.get("role").and_then(|r| r.as_str()) == Some("user");
        let content_str = msg.get("content").and_then(|c| c.as_str());

        if is_user && content_str.is_some() {
            let cur_text = content_str.unwrap();
            if let Some(prev) = normalized_messages.last_mut() {
                if prev.get("role").and_then(|r| r.as_str()) == Some("user") && prev.get("content").and_then(|c| c.as_str()).is_some() {
                    let prev_text = prev["content"].as_str().unwrap().to_string();
                    let merged = format!("{}\n\n{}", prev_text, cur_text);
                    prev["content"] = Value::String(merged);
                    continue;
                }
            }
        }
        normalized_messages.push(msg);
    }
    // Context Governor: protect against hitting llama.cpp / LM Studio hard context ceiling (n_ctx ~6582)
    // Budget max ~12,000 characters backwards so prompt_tokens stays around ~4,000,
    // leaving 2,500+ tokens of KV cache completely free for generation!
    const BUDGET_CHARS: usize = 75000; // Scaled for loaded 32,768 context length
    let mut governed_messages: Vec<Value> = Vec::new();
    let mut curr_chars = 0usize;

    for msg in normalized_messages.into_iter().rev() {
        let c_len = if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
            s.len()
        } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
            arr.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str())).map(|s| s.len()).sum()
        } else {
            0
        };

        if curr_chars + c_len > BUDGET_CHARS && governed_messages.len() >= 3 {
            break;
        }
        curr_chars += c_len;
        governed_messages.push(msg);
    }
    governed_messages.reverse();

    // Ensure the conversation always starts with a user role message for template compliance
    if let Some(first) = governed_messages.first() {
        if first.get("role").and_then(|r| r.as_str()) != Some("user") {
            governed_messages.insert(0, json!({
                "role": "user",
                "content": "[Context continuation from earlier conversation turns]"
            }));
        }
    }

    // If the conversation ends with tool execution results, guarantee a user synthesis directive
    // so Qwen 2.5 / llama.cpp Jinja template never emits an immediate empty EOS response!
    if let Some(last) = governed_messages.last() {
        if last.get("role").and_then(|r| r.as_str()) == Some("tool") {
            governed_messages.push(json!({
                "role": "user",
                "content": "[DIRECTIVE]: Tool execution completed. Review the gathered data above, synthesize the facts, and formulate your comprehensive answer now."
            }));
        }
    }

    let payload_messages = governed_messages;

    let mut body = json!({
        "model": model,
        "messages": payload_messages,
        "temperature": 0.3,
        "top_p": 0.9,
        "max_tokens": 16384,
        "stream": true,
    });

    if enable_tools {
        let tools = if let Some(tool_name) = &forced_tool {
            let all = build_tool_definitions();
            let filtered: Vec<Value> = all.into_iter().filter(|t| {
                t.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) == Some(tool_name.as_str())
            }).collect();
            if filtered.is_empty() { build_tool_definitions() } else { filtered }
        } else {
            build_tool_definitions()
        };

        body["tools"] = Value::Array(tools);
        body["tool_choice"] = Value::String(if forced_tool.is_some() { "required".to_string() } else { "auto".to_string() });
    }

    if let Ok(pretty) = serde_json::to_string_pretty(&body) {
        let _ = tx.send(StreamUpdate::Payload(pretty));
    }

    let start_time = std::time::Instant::now();
    let num_tools = if enable_tools {
        body.get("tools").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0)
    } else {
        0
    };

    let _ = tx.send(StreamUpdate::Log(format!(
        "🚀 [API Req] Model: {} | Messages: {} | Tools: {}",
        model, payload_messages.len(), num_tools
    )));

    for (i, m) in payload_messages.iter().enumerate() {
        let r = m.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let (content_len, preview) = if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
            let one_line = s.lines().next().unwrap_or("").trim();
            let p = if one_line.chars().count() > 40 {
                format!("{}...", one_line.chars().take(37).collect::<String>())
            } else {
                one_line.to_string()
            };
            (s.len(), p)
        } else if let Some(arr) = m.get("content").and_then(|v| v.as_array()) {
            let num_parts = arr.len();
            let num_images = arr.iter().filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("image_url")).count();
            let text_preview = arr.iter().find_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            }).unwrap_or("");
            let approx_len = serde_json::to_string(arr).map(|s| s.len()).unwrap_or(0);
            (approx_len, format!("[{} parts, {} img] {}", num_parts, num_images, text_preview))
        } else {
            (0, String::new())
        };

        let _ = tx.send(StreamUpdate::Log(format!(
            "   [{}] {:<9} ({} bytes) \"{}\"",
            i, r, content_len, preview
        )));
    }

    let mut req = client.post(&url)
        .header("Content-Type", "application/json")
        .json(&body);

    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", key.trim()));
        }
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let err_msg = format!("Network Error connecting to {}: {}", url, e);
            let _ = tx.send(StreamUpdate::Log(format!("❌ [API Error] {}", err_msg)));
            let _ = tx.send(StreamUpdate::Error(err_msg));
            return;
        }
    };

    let ttft_ms = start_time.elapsed().as_millis();
    let status_code = resp.status();
    let _ = tx.send(StreamUpdate::Log(format!(
        "📡 [API Resp] HTTP {} | TTFT: {}ms",
        status_code, ttft_ms
    )));

    if !resp.status().is_success() {
        let err_msg = format!("Server returned HTTP status: {}", resp.status());
        let _ = tx.send(StreamUpdate::Log(format!("❌ [API Error] {}", err_msg)));
        let _ = tx.send(StreamUpdate::Error(err_msg));
        return;
    }

    let mut byte_stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut stream_parser = StreamParser::new();
    let mut tool_calls_map: HashMap<usize, (String, String, String)> = HashMap::new();
    let mut raw_full_output = String::new();
    let mut last_finish_reason: Option<String> = None;
    let mut usage_info: Option<(u64, u64, u64)> = None;
    let mut chunk_count = 0usize;

    while let Some(item) = byte_stream.next().await {
        if cancel_token.is_cancelled() {
            let _ = tx.send(StreamUpdate::Log("🛑 [API Cancelled] Generation cancelled by user".to_string()));
            let _ = tx.send(StreamUpdate::Error("[Generation cancelled by user]".to_string()));
            return;
        }

        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let err_msg = format!("Stream read error: {}", e);
                let _ = tx.send(StreamUpdate::Log(format!("❌ [Stream Error] {}", err_msg)));
                let _ = tx.send(StreamUpdate::Error(err_msg));
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(nl_pos) = buffer.find('\n') {
            let line = buffer[..nl_pos].trim().to_string();
            buffer = buffer[nl_pos + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" {
                continue;
            }

            if let Some(json_str) = line.strip_prefix("data: ") {
                if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                    if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
                        if let Some(first_choice) = choices.get(0) {
                            if let Some(fr) = first_choice.get("finish_reason").and_then(|f| f.as_str()) {
                                last_finish_reason = Some(fr.to_string());
                            }
                        }
                    }
                    if let Some(usage) = val.get("usage") {
                        let pt = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let ct = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let tt = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        usage_info = Some((pt, ct, tt));
                    }

                    if let Some(delta) = val.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("delta")) {
                        // 1. Direct reasoning_content field
                        if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
                            if !reasoning.is_empty() {
                                chunk_count += 1;
                                stream_parser.accumulated_thinking.push_str(reasoning);
                                let _ = tx.send(StreamUpdate::Thinking(reasoning.to_string()));
                            }
                        }

                        // 2. Native tool_calls chunks
                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tool_calls {
                                chunk_count += 1;
                                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                                let entry = tool_calls_map.entry(idx).or_insert_with(|| (String::new(), String::new(), String::new()));
                                if let Some(call_id) = tc.get("id").and_then(|s| s.as_str()) {
                                    entry.0.push_str(call_id);
                                }
                                if let Some(fn_obj) = tc.get("function") {
                                    if let Some(n) = fn_obj.get("name").and_then(|s| s.as_str()) {
                                        entry.1.push_str(n);
                                    }
                                    if let Some(a) = fn_obj.get("arguments").and_then(|s| s.as_str()) {
                                        entry.2.push_str(a);
                                        let _ = tx.send(StreamUpdate::ToolDrafting(a.to_string()));
                                    }
                                }
                            }
                        }

                        // 3. Content tokens processed through stream_parser (filters out <think> and tool tags from screen!)
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                chunk_count += 1;
                                raw_full_output.push_str(content);
                                let parsed_events = stream_parser.push_chunk(content);
                                for ev in parsed_events {
                                    match ev {
                                        ParsedChunk::Content(clean_txt) => {
                                            let _ = tx.send(StreamUpdate::Token(clean_txt));
                                        }
                                        ParsedChunk::Thinking(think_txt) => {
                                            let _ = tx.send(StreamUpdate::Thinking(think_txt));
                                        }
                                        ParsedChunk::ToolDraft(draft_txt) => {
                                            let _ = tx.send(StreamUpdate::ToolDrafting(draft_txt));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let elapsed = start_time.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let tps = if elapsed.as_secs_f64() > 0.05 && chunk_count > 0 {
        chunk_count as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    let finish_str = last_finish_reason.as_deref().unwrap_or("unknown");
    if let Some((pt, ct, tt)) = usage_info {
        let _ = tx.send(StreamUpdate::Log(format!(
            "🏁 [Stream Done] finish: \"{}\" | prompt: {} tok | completion: {} tok | total: {} tok | {}ms ({:.1} t/s)",
            finish_str, pt, ct, tt, elapsed_ms, tps
        )));
        if ct == 0 && finish_str == "stop" {
            let _ = tx.send(StreamUpdate::Log(
                "⚠️ [WARN] 0 completion tokens emitted with finish_reason='stop'! Prompt may exceed model context window or triggered stop token.".to_string()
            ));
        }
    } else {
        let _ = tx.send(StreamUpdate::Log(format!(
            "🏁 [Stream Done] finish: \"{}\" | chunks: {} | {}ms ({:.1} t/s)",
            finish_str, chunk_count, elapsed_ms, tps
        )));
    }

    let thinking_opt = if !stream_parser.accumulated_thinking.is_empty() {
        Some(stream_parser.accumulated_thinking.clone())
    } else {
        None
    };

    // 1. Finalize tool calls only if enabled
    let mut final_tool_calls = Vec::new();

    if enable_tools {
        let mut sorted_keys: Vec<_> = tool_calls_map.keys().copied().collect();
        sorted_keys.sort();
        for k in sorted_keys {
            if let Some((id, name, args)) = tool_calls_map.remove(&k) {
                if !name.is_empty() {
                    let call_id = if id.is_empty() {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        id
                    };
                    let norm_args = clean_and_normalize_tool_arguments(&name, &args);
                    final_tool_calls.push(ToolCallInfo {
                        id: call_id,
                        name,
                        arguments: norm_args,
                    });
                }
            }
        }

        // 2. If no native tool calls, extract from raw text (handles <tool_call>, <invoke>, <function=...>, ```tool_call)
        if final_tool_calls.is_empty() {
            final_tool_calls = extract_text_tool_calls(&raw_full_output);
            if final_tool_calls.is_empty() && !stream_parser.accumulated_tool_draft.is_empty() {
                final_tool_calls = extract_text_tool_calls(&stream_parser.accumulated_tool_draft);
            }
            if final_tool_calls.is_empty() {
                if let Some(th) = &thinking_opt {
                    final_tool_calls = extract_text_tool_calls(th);
                }
            }
        }
    }

    let clean_content = strip_tool_tags(&stream_parser.accumulated_content);

    let _ = tx.send(StreamUpdate::Finished {
        full_content: clean_content,
        full_thinking: thinking_opt,
        tool_calls: final_tool_calls,
    });
}





#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_clean_parse() {
        let raw = "<path>C:\\Windows\\Temp\\tuol_test.txt</path>\n<content>line1: write_file OK 2026-10-24 Moscow\nline2: search_files found fn main at src/main.rs:11\nline3: all 8 tools verified\n</content>";
        let norm_str = clean_and_normalize_tool_arguments("write_file", raw);
        let parsed: serde_json::Value = serde_json::from_str(&norm_str).unwrap();
        assert_eq!(parsed.get("path").and_then(|v| v.as_str()), Some("C:\\Windows\\Temp\\tuol_test.txt"));
        assert!(parsed.get("content").unwrap().as_str().unwrap().contains("line1: write_file OK"));
        println!("PASSED XML WRITE_FILE! NORM: {}", norm_str);
    }
}
