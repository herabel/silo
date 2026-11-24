use crate::engine::client::clean_and_normalize_tool_arguments;
use crate::session::{ChatMessage, MessageRole};
use serde_json::{json, Value};

/// Dynamically computed token and character budget scaled to the model's actual context window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextBudget {
    pub context_window_tokens: usize,
    pub max_output_tokens: usize,
    pub prompt_token_budget: usize,
    pub prompt_char_budget: usize,
    pub hot_tool_cap: usize,
    pub warm_tool_cap: usize,
    pub cold_tool_cap: usize,
}

impl ContextBudget {
    pub fn compute(
        context_window_tokens: usize,
        max_output_tokens: usize,
        enable_tools: bool,
    ) -> Self {
        let tool_schema_overhead = if enable_tools { 1500 } else { 0 };
        let prompt_token_budget = context_window_tokens
            .saturating_sub(max_output_tokens)
            .saturating_sub(tool_schema_overhead)
            .max(1024);

        // Conservative character budget: Rust source code, tool JSON, and punctuation
        // average ~2.2 characters per token in BPE tokenizers.
        // Using 2.2 chars/token guarantees we never trigger n_ctx overflow.
        let prompt_char_budget = (prompt_token_budget as f32 * 2.2) as usize;

        // Dynamic caps scaling dynamically with context window:
        // Hot tools (latest 2 turns): up to 25% of prompt budget, clamped at [1000, 3500]
        let hot_tool_cap = (prompt_char_budget / 4).clamp(1000, 3500);
        // Warm tools (turns 2-3 ago): up to 5% of prompt budget, clamped at [250, 800]
        let warm_tool_cap = (prompt_char_budget / 20).clamp(250, 800);
        // Cold tools (turns 4+ ago): up to 1.5% of prompt budget, clamped at [80, 250]
        let cold_tool_cap = (prompt_char_budget / 70).clamp(80, 250);

        Self {
            context_window_tokens,
            max_output_tokens,
            prompt_token_budget,
            prompt_char_budget,
            hot_tool_cap,
            warm_tool_cap,
            cold_tool_cap,
        }
    }
}

/// Filter out repetitive compiler warning noise (e.g. egui float fallback warnings)
/// while strictly preserving errors, test results, and compiler status.
fn filter_compiler_noise(raw: &str) -> String {
    let mut kept_lines: Vec<String> = Vec::new();
    let mut skip_warning = false;
    let mut warning_count = 0usize;

    for line in raw.lines() {
        if line.starts_with("warning: falling back to `f32`") {
            skip_warning = true;
            warning_count += 1;
            continue;
        }
        if skip_warning {
            if line.starts_with("warning:")
                || line.starts_with("error")
                || line.starts_with("   Compiling")
                || line.starts_with("test ")
                || line.starts_with("running ")
                || line.starts_with("Finished")
            {
                skip_warning = false;
            } else {
                continue;
            }
        }
        kept_lines.push(line.to_string());
    }

    if warning_count > 0 {
        kept_lines.insert(
            0,
            format!("[Note: {} repetitive compiler warnings filtered]", warning_count),
        );
    }

    kept_lines.join("\n")
}

/// Compact tool output based on tool-aware semantics, recency, and context budget.
pub fn compact_tool_output(
    content: &str,
    tool_name: &str,
    age: usize,
    budget: &ContextBudget,
) -> String {
    let lower_tool = tool_name.to_lowercase();

    // 1. Absolute immunity for working memory: scratchpad is NEVER compacted.
    if lower_tool == "scratchpad" || lower_tool == "notes" || lower_tool == "memo" {
        return content.to_string();
    }

    // 2. Pre-filter repetitive compiler warning noise for command runs
    let cleaned_content = if lower_tool == "run_command" && content.contains("warning: falling back to `f32`") {
        filter_compiler_noise(content)
    } else {
        content.to_string()
    };
    let char_count = cleaned_content.chars().count();

    // 3. High immunity for search indexing & directory listings (up to 1200 chars)
    if (lower_tool == "search_files" || lower_tool == "list_dir" || lower_tool == "git_status")
        && char_count <= 1200
        && age <= 4
    {
        return cleaned_content;
    }

    // 4. General semantic protection for short, dense outputs (<= 600 chars)
    if char_count <= 600 && age <= 4 {
        return cleaned_content;
    }

    let target_cap = if age <= 1 {
        budget.hot_tool_cap
    } else if age <= 3 {
        budget.warm_tool_cap
    } else {
        budget.cold_tool_cap
    };

    if char_count <= target_cap {
        return cleaned_content;
    }

    if age <= 1 {
        // Hot turn: retain head up to target_cap
        let take_len = target_cap.saturating_sub(60).max(100);
        let head: String = cleaned_content.chars().take(take_len).collect();
        format!(
            "{}\n[... {} chars truncated to fit active context budget ...]",
            head,
            char_count.saturating_sub(head.chars().count())
        )
    } else if age <= 3 {
        // Warm turn: head + tail with omission notice
        let effective_cap = target_cap.saturating_sub(60);
        let head_len = (effective_cap * 6) / 10;
        let tail_len = (effective_cap * 4) / 10;
        let head: String = cleaned_content.chars().take(head_len).collect();
        let tail: String = cleaned_content
            .chars()
            .skip(char_count.saturating_sub(tail_len))
            .collect();
        let omitted = char_count.saturating_sub(head_len + tail_len);
        format!(
            "{}\n[... {} chars omitted for context budget ...]\n{}",
            head, omitted, tail
        )
    } else {
        // Cold turn: compact to 3-4 preview lines + reminder to check scratchpad
        let preview: String = cleaned_content
            .lines()
            .take(4)
            .collect::<Vec<&str>>()
            .join("\n");
        let preview_cut: String = preview.chars().take(target_cap.saturating_sub(80)).collect();
        format!(
            "{}\n[... historical output compacted ({} chars total). Key facts preserved in scratchpad]",
            preview_cut, char_count
        )
    }
}

/// Builds OpenAI-compatible JSON payload messages with adaptive rolling compaction.
/// Ensures tool calls and tool responses remain strictly paired, the initial user/system
/// prompt is never dropped, and the total character footprint respects `context_window_tokens`.
pub fn build_governed_payload_messages(
    messages: &[ChatMessage],
    system_prompt: &str,
    context_window_tokens: usize,
    max_output_tokens: usize,
    enable_tools: bool,
) -> Vec<Value> {
    let budget = ContextBudget::compute(context_window_tokens, max_output_tokens, enable_tools);
    let mut payload_messages = Vec::new();
    let sys_trimmed = system_prompt.trim();

    let first_user_idx = messages.iter().position(|m| m.role == MessageRole::User);

    if first_user_idx.is_none() && !sys_trimmed.is_empty() {
        payload_messages.push(json!({
            "role": "user",
            "content": sys_trimmed
        }));
    }

    // Identify tool messages and their chronological positions
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == MessageRole::Tool)
        .map(|(i, _)| i)
        .collect();
    let total_tools = tool_indices.len();

    // Watermark check: Calculate uncompressed message footprint.
    // If the conversation is comfortably in the green zone (<= 70% of budget),
    // preserve full fidelity without rolling compaction so the model remembers every detail.
    let raw_char_sum: usize = messages.iter().map(|m| m.content.len()).sum();
    let watermark_triggered = raw_char_sum > (budget.prompt_char_budget * 7) / 10;

    let last_text_assistant_idx = messages.iter().rposition(|m| {
        m.role == MessageRole::Assistant
            && !m.content.trim().is_empty()
            && !m.content.trim().starts_with("⚠️")
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
                if trimmed.starts_with("⚠️")
                    || trimmed.starts_with("*(Model returned an empty response")
                    || trimmed.starts_with("*(Model reasoned internally")
                {
                    continue;
                }

                let is_superseded =
                    last_text_assistant_idx.is_some_and(|last_idx| idx < last_idx);

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
                        let safe_args =
                            clean_and_normalize_tool_arguments(&tc.name, &tc.arguments);
                        tc_objs.push(json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": safe_args
                            }
                        }));
                    }
                    payload_messages.push(json!({
                        "role": "assistant",
                        "content": if trimmed.is_empty() { Value::String(String::new()) } else { Value::String(trimmed.to_string()) },
                        "tool_calls": tc_objs
                    }));
                } else if !trimmed.is_empty() {
                    payload_messages.push(json!({
                        "role": "assistant",
                        "content": trimmed
                    }));
                }
            }
            MessageRole::System => {
                let trimmed = m.content.trim();
                if !trimmed.is_empty() {
                    payload_messages.push(json!({
                        "role": "user",
                        "content": trimmed
                    }));
                }
            }
            MessageRole::Tool => {
                let call_id = m
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_default".to_string());

                let tool_pos = tool_indices.iter().position(|&i| i == idx).unwrap_or(0);
                let age = total_tools.saturating_sub(1 + tool_pos);

                // Find tool name from preceding assistant messages matching call_id
                let tool_name = messages
                    .iter()
                    .filter_map(|msg| {
                        msg.tool_calls
                            .iter()
                            .find(|tc| tc.id == call_id)
                            .map(|tc| tc.name.as_str())
                    })
                    .next()
                    .unwrap_or("unknown");

                let compacted = if watermark_triggered {
                    compact_tool_output(&m.content, tool_name, age, &budget)
                } else if m.content.chars().count() > budget.hot_tool_cap {
                    compact_tool_output(&m.content, tool_name, 0, &budget)
                } else {
                    m.content.clone()
                };

                payload_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": compacted
                }));
            }
        }
    }

    // Normalize consecutive user messages
    let mut normalized_messages: Vec<Value> = Vec::new();
    for msg in payload_messages {
        let is_user = msg.get("role").and_then(|r| r.as_str()) == Some("user");
        let content_str = msg.get("content").and_then(|c| c.as_str());

        if is_user && content_str.is_some() {
            let cur_text = content_str.unwrap();
            if let Some(prev) = normalized_messages.last_mut() {
                if prev.get("role").and_then(|r| r.as_str()) == Some("user")
                    && prev.get("content").and_then(|c| c.as_str()).is_some()
                {
                    let prev_text = prev["content"].as_str().unwrap().to_string();
                    let merged = format!("{}\n\n{}", prev_text, cur_text);
                    prev["content"] = Value::String(merged);
                    continue;
                }
            }
        }
        normalized_messages.push(msg);
    }

    // Calculate total character footprint across message bodies
    let calc_total_chars = |msgs: &[Value]| -> usize {
        msgs.iter()
            .map(|msg| {
                if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                    s.len()
                } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                    arr.iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .map(|s| s.len())
                        .sum()
                } else {
                    0
                }
            })
            .sum()
    };

    let mut total_chars = calc_total_chars(&normalized_messages);

    // If still over budget, evict cold tool content down to a minimal stub
    if total_chars > budget.prompt_char_budget {
        for msg in normalized_messages.iter_mut() {
            if total_chars <= budget.prompt_char_budget {
                break;
            }
            if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                    if content.len() > 100 {
                        let stub = "[Historical tool output evicted to preserve context window. Refer to scratchpad.]";
                        let freed = content.len().saturating_sub(stub.len());
                        *msg.get_mut("content").unwrap() = Value::String(stub.to_string());
                        total_chars = total_chars.saturating_sub(freed);
                    }
                }
            }
        }
    }

    // Ensure conversation starts with user message
    if let Some(first) = normalized_messages.first() {
        if first.get("role").and_then(|r| r.as_str()) != Some("user") {
            normalized_messages.insert(
                0,
                json!({
                    "role": "user",
                    "content": "[Context continuation from earlier conversation turns]"
                }),
            );
        }
    }

    normalized_messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ChatMessage, ToolCallRecord, ToolStatus};

    #[test]
    fn test_context_budget_dynamic_scaling() {
        // Test 4k, 8k, 16k, 32k contexts
        let b4k = ContextBudget::compute(4096, 1024, true);
        let b8k = ContextBudget::compute(8192, 2048, true);
        let b16k = ContextBudget::compute(16384, 4096, true);
        let b32k = ContextBudget::compute(32768, 4096, true);

        // Verification of monotonic scaling
        assert!(b4k.prompt_char_budget < b8k.prompt_char_budget);
        assert!(b8k.prompt_char_budget < b16k.prompt_char_budget);
        assert!(b16k.prompt_char_budget < b32k.prompt_char_budget);

        // Hot cap is always >= warm cap >= cold cap
        for b in &[b4k, b8k, b16k, b32k] {
            assert!(b.hot_tool_cap >= b.warm_tool_cap);
            assert!(b.warm_tool_cap >= b.cold_tool_cap);
        }

        // On 16k context, hot cap should be 3500 chars, warm cap ~800, cold ~250
        assert_eq!(b16k.hot_tool_cap, 3500);
        assert_eq!(b16k.warm_tool_cap, 800);
        assert_eq!(b16k.cold_tool_cap, 250);
    }

    #[test]
    fn test_compact_tool_output_recency_levels() {
        let budget = ContextBudget::compute(16384, 4096, true);
        let sample = "Line 1: Status OK\nLine 2: Target found\nLine 3: Reading data\n".repeat(70);
        assert!(sample.len() > 3000);

        // Hot turn (age 0): up to hot cap with truncated marker
        let hot = compact_tool_output(&sample, "read_file", 0, &budget);
        assert!(hot.len() <= budget.hot_tool_cap + 10);
        assert!(hot.contains("truncated to fit active context budget"));

        // Warm turn (age 2): head + tail with omission notice
        let warm = compact_tool_output(&sample, "read_file", 2, &budget);
        assert!(warm.len() <= budget.warm_tool_cap + 10);
        assert!(warm.contains("omitted for context budget"));

        // Cold turn (age 5): preview lines + scratchpad reminder
        let cold = compact_tool_output(&sample, "read_file", 5, &budget);
        assert!(cold.len() <= budget.cold_tool_cap + 10);
        assert!(cold.contains("Key facts preserved in scratchpad"));

        // Scratchpad immunity test: even at age 10 with 5000 chars, scratchpad is NEVER compacted!
        let pad = compact_tool_output(&sample, "scratchpad", 10, &budget);
        assert_eq!(pad.len(), sample.len());
        assert_eq!(pad, sample);

        // Search files immunity test: up to 1200 chars at age 2 is untouched
        let search_hits = "src/main.rs:10: fn run()\nsrc/lib.rs:25: fn init()\n".repeat(20);
        assert!(search_hits.len() < 1200);
        let search_governed = compact_tool_output(&search_hits, "search_files", 2, &budget);
        assert_eq!(search_governed, search_hits);

        // Compiler noise test: repetitive warnings are cleanly stripped
        let compiler_spam = "warning: falling back to `f32` as the trait bound `f32: From<f64>` is not satisfied\n   --> src/app.rs:264:64\n\nwarning: falling back to `f32` as the trait bound `f32: From<f64>` is not satisfied\n   --> src/app.rs:268:58\n\ntest result: ok. 10 passed; 0 failed;\n";
        let cleaned = compact_tool_output(compiler_spam, "run_command", 0, &budget);
        assert!(cleaned.contains("test result: ok"));
        assert!(cleaned.contains("repetitive compiler warnings filtered"));
    }

    #[test]
    fn test_governed_messages_tool_pairing_and_budget() {
        let system_prompt = "You are Silo systems agent.";
        let user_prompt = "Refactor the codebase";

        let mut messages = vec![ChatMessage::user(user_prompt)];

        // Simulate 12 turns of tool calls with 3500 chars output each (like the actual Inkling run)
        for i in 0..12 {
            let call_id = format!("call_{}", i);
            let mut asst = ChatMessage::assistant("", None);
            asst.tool_calls.push(ToolCallRecord {
                id: call_id.clone(),
                name: "run_command".to_string(),
                arguments: format!("{{\"cmd\": \"cat file_{}.rs\"}}", i),
                output: None,
                status: ToolStatus::Success,
            });
            messages.push(asst);

            let tool_output = format!("File {} header\nfn process_{}() {{ ... }}\n", i, i).repeat(100);
            let mut tool_msg = ChatMessage::tool(&tool_output, &call_id);
            tool_msg.tool_call_id = Some(call_id);
            messages.push(tool_msg);
        }

        // Build governed messages for 16k context
        let governed = build_governed_payload_messages(&messages, system_prompt, 16384, 4096, true);

        // 1. Initial user prompt with system prompt MUST be preserved
        let first = governed.first().unwrap();
        assert_eq!(first["role"], "user");
        let first_text = first["content"].as_str().unwrap();
        assert!(first_text.contains(system_prompt));
        assert!(first_text.contains(user_prompt));

        // 2. Every tool message has a valid preceding assistant message with matching tool_call id
        for (idx, msg) in governed.iter().enumerate() {
            if msg["role"] == "tool" {
                let call_id = msg["tool_call_id"].as_str().unwrap();
                let prev_asst = &governed[idx - 1];
                assert_eq!(prev_asst["role"], "assistant");
                let tc_arr = prev_asst["tool_calls"].as_array().unwrap();
                assert!(tc_arr.iter().any(|tc| tc["id"] == call_id));
            }
        }

        // 3. Total character footprint is strictly within the 16k budget
        let total_chars: usize = governed
            .iter()
            .map(|m| m["content"].as_str().unwrap_or("").len())
            .sum();
        let budget16k = ContextBudget::compute(16384, 4096, true);
        assert!(
            total_chars <= budget16k.prompt_char_budget,
            "Total chars {} exceeded budget {}",
            total_chars,
            budget16k.prompt_char_budget
        );
        println!("12-step test: total chars = {} (budget = {})", total_chars, budget16k.prompt_char_budget);
    }

    #[test]
    fn test_watermark_preserves_full_fidelity_when_under_threshold() {
        let system_prompt = "You are Silo.";
        let user_prompt = "Brief query";
        let mut messages = vec![ChatMessage::user(user_prompt)];

        // 3 tool turns with modest output (~400 chars each = ~1200 chars total, well under 70% watermark)
        for i in 0..3 {
            let call_id = format!("call_{}", i);
            let mut asst = ChatMessage::assistant("", None);
            asst.tool_calls.push(ToolCallRecord {
                id: call_id.clone(),
                name: "search_files".to_string(),
                arguments: "{}".to_string(),
                output: None,
                status: ToolStatus::Success,
            });
            messages.push(asst);

            let tool_output = format!("src/main.rs:{}: fn target_{}() {{ let x = {}; }}", i * 10, i, i);
            let mut tool_msg = ChatMessage::tool(&tool_output, &call_id);
            tool_msg.tool_call_id = Some(call_id);
            messages.push(tool_msg);
        }

        let governed = build_governed_payload_messages(&messages, system_prompt, 16384, 4096, true);
        // Since we are under the 70% watermark, every tool output should remain 100% intact!
        for (_idx, msg) in governed.iter().enumerate() {
            if msg["role"] == "tool" {
                let content = msg["content"].as_str().unwrap();
                assert!(content.contains("fn target_"));
                assert!(!content.contains("compacted"));
                assert!(!content.contains("truncated"));
            }
        }
    }
}
