use crate::engine::{execute_tool, fetch_available_models, stream_completion, StreamUpdate};
use crate::runtime_env::{build_configured_system_prompt, build_system_prompt, mask_output_data};
use crate::session::{ChatMessage, ContextConfig, ToolCallRecord, ToolStatus};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

const DEEP_RESEARCH_DIRECTIVE: &str = r#"DEEP RESEARCH DIRECTIVE:
You are an investigative technical analyst. Follow a rigorous multi-phase research approach:
1. IMMEDIATE TOOL DELEGATION: Never perform mental arithmetic, formula derivations, or parameter checks in <think>. Immediately offload all math, size calculations, and data processing to `execute_python`.
2. Scout: Run multiple `web_search` queries covering technical specifications, standards, and data.
3. Read: Call `fetch_page(url)` on relevant URLs from search results to inspect source documents.
4. Compute: Validate mathematical models or verify equations using `execute_python`.
5. STRICT TOOL LOOP ENFORCEMENT: Never output conversational filler or unexecuted promises in chat (such as "Let me fetch...", "I will search...", "Now checking..."). Always invoke the tool call directly. Only output natural language markdown when synthesizing the final comprehensive dossier.
6. Synthesize: Formulate an exhaustive, highly structured technical dossier with citations and data tables."#;

pub async fn run_headless(args: Vec<String>) {
    let mut prompt_arg: Option<String> = None;
    let mut deep_arg: Option<String> = None;
    let mut eval_arg: Option<String> = None;
    let mut model_arg: Option<String> = None;
    let mut verbose = false;
    let mut test_tools = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--verbose" | "-v" => {
                verbose = true;
            }
            "--test-tools" | "-t" => {
                test_tools = true;
            }
            "--prompt" | "-p" => {
                if i + 1 < args.len() {
                    prompt_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--deep" | "-d" => {
                if i + 1 < args.len() {
                    deep_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--eval" | "-e" => {
                if i + 1 < args.len() {
                    eval_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--model" | "-m" => {
                if i + 1 < args.len() {
                    model_arg = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--help" => {
                print_help();
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if test_tools {
        run_tool_diagnostics().await;
        return;
    }

    if let Some(code) = eval_arg {
        println!("=== Direct Python Eval ===");
        println!("Code:\n{}", code);
        let (ok, out) = execute_tool("execute_python", &format!("{{\"code\": {:?}}}", code)).await;
        println!("Status: {}", if ok { "SUCCESS" } else { "FAILED" });
        println!("Output:\n{}", out);
        return;
    }

    let is_deep = deep_arg.is_some();
    let topic_or_prompt = deep_arg.or(prompt_arg);

    if let Some(user_query) = topic_or_prompt {
        run_agent_session(&user_query, is_deep, model_arg.as_deref(), verbose).await;
    } else {
        print_help();
    }
}

fn print_help() {
    println!("Silo Headless Runner & Debugger");
    println!("Usage:");
    println!(
        "  silo.exe --headless --test-tools           Run diagnostics on all tools and parser"
    );
    println!("  silo.exe --headless --eval \"<python_code>\" Directly evaluate Python code");
    println!("  silo.exe --headless --prompt \"<prompt>\"    Run interactive turn with local LLM");
    println!(
        "  silo.exe --headless --deep \"<topic>\"       Run autonomous multi-step deep research"
    );
    println!("  silo.exe --headless --model \"<model_id>\"   Override target model ID");
}

pub async fn run_tool_diagnostics() {
    println!("==================================================");
    println!("       SILO — TOOL & PARSER DIAGNOSTICS SUITE     ");
    println!("==================================================");

    // 1. LM Studio Connectivity & Models
    print!("[1/7] Checking Inference Server (http://127.0.0.1:1234)... ");
    let models = fetch_available_models("http://127.0.0.1:1234", None).await;
    if models.is_empty() {
        println!("FAIL! (No models detected or server not running)");
    } else {
        println!("OK! Found {} models:", models.len());
        for m in &models {
            println!("      • {} (loaded: {})", m.id, m.is_loaded);
        }
    }

    // 2. Python Execution
    print!("[2/7] Testing Python execution ('execute_python')... ");
    let (ok, out) = execute_tool("execute_python", "{\"code\": \"print(2**64 - 1)\"}").await;
    if ok && out.contains("18446744073709551615") {
        println!("OK! Result: {}", out.trim());
    } else {
        println!("FAIL! (ok={}, output={})", ok, out);
    }

    // 3. Shell Command Execution
    print!("[3/7] Testing Shell execution ('run_command')... ");
    let (ok, out) = execute_tool("run_command", "{\"cmd\": \"echo Silo_Host_OK\"}").await;
    if ok && out.contains("Silo_Host_OK") {
        println!("OK! Output: {}", out.trim());
    } else {
        println!("FAIL! (ok={}, output={})", ok, out);
    }

    // 4. Web Search
    print!("[4/7] Testing Web Search ('web_search')... ");
    let (ok, out) = execute_tool("web_search", "{\"query\": \"Rust programming language\"}").await;
    if ok && out.starts_with('[') && out.len() > 20 {
        let count = out.matches("\"url\"").count();
        println!("OK! Found {} search results.", count);
    } else {
        println!("FAIL! (ok={}, output={})", ok, out);
    }

    // 5. Web Scout Fetch
    print!("[5/7] Testing Web Scout Fetch ('fetch_page')... ");
    let (ok, out) = execute_tool(
        "fetch_page",
        "{\"url\": \"https://en.wikipedia.org/wiki/Rust_(programming_language)\"}",
    )
    .await;
    if ok && out.len() > 100 {
        println!("OK! Fetched {} chars of markdown.", out.len());
    } else {
        println!("FAIL! (ok={}, output={})", ok, out);
    }

    // 6. FS Tools Verification
    print!("[6/9] Testing FS Tools ('write_file', 'read_file', 'list_dir', 'search_files')... ");
    let test_path = "target/fs_tool_test.txt";
    let test_content = "line one\nline two with KEYWORD\nline three\nline four";
    let (w_ok, _) = execute_tool(
        "write_file",
        &format!(
            "{{\"path\": {:?}, \"content\": {:?}}}",
            test_path, test_content
        ),
    )
    .await;
    let (r_ok, r_out) = execute_tool(
        "read_file",
        &format!(
            "{{\"path\": {:?}, \"start_line\": 2, \"end_line\": 3}}",
            test_path
        ),
    )
    .await;
    let (l_ok, l_out) = execute_tool("list_dir", "{\"path\": \"target\", \"depth\": 1}").await;
    let (s_ok, s_out) = execute_tool(
        "search_files",
        &format!("{{\"pattern\": \"KEYWORD\", \"path\": {:?}}}", test_path),
    )
    .await;
    let _ = std::fs::remove_file(test_path);

    if w_ok
        && r_ok
        && r_out.contains("2: line two")
        && l_ok
        && l_out.contains("[FILE]")
        && s_ok
        && s_out.contains("KEYWORD")
    {
        println!("OK! All 4 FS tools passed with line slicing and search.");
    } else {
        println!(
            "FAIL! (w_ok={}, r_ok={}, l_ok={}, s_ok={})",
            w_ok, r_ok, l_ok, s_ok
        );
    }

    // 7. Tool Tag Extraction Tests
    print!("[7/9] Testing Tool Tag Extraction Formats... ");
    let sample_text = r#"
Some preliminary thought here.
<tool_call>
{"name": "web_search", "arguments": "{\"query\": \"test tag\"}"}
</tool_call>
"#;
    let extracted = crate::engine::extract_text_tool_calls(sample_text);
    if extracted.len() == 1 && extracted[0].name == "web_search" {
        println!(
            "OK! Extracted: {} with query: {}",
            extracted[0].name, extracted[0].arguments
        );
    } else {
        println!("FAIL! Extracted {} calls", extracted.len());
    }

    // 8. Tool Function Call Paren Syntax Tests
    print!("[8/9] Testing Function-Call Paren Parsing (e.g. read_file(path=...))... ");
    let paren_call_text = r#"<tool_call>
<function=read_file(path: "src/main.rs", start_line: 10, end_line: 25)
</parameter>
</function>
</tool_call>"#;
    let extracted_paren = crate::engine::extract_text_tool_calls(paren_call_text);
    if extracted_paren.len() == 1
        && extracted_paren[0].name == "read_file"
        && extracted_paren[0].arguments.contains("\"start_line\":10")
    {
        println!(
            "OK! Parsed paren call into JSON: {}",
            extracted_paren[0].arguments
        );
    } else {
        println!(
            "FAIL! (len={}, args={:?})",
            extracted_paren.len(),
            extracted_paren.first().map(|x| &x.arguments)
        );
    }

    // 9. Context Spoofing & Output Sanitizer Tests
    print!("[9/9] Testing Context Spoofing & Output Sanitizer... ");
    let mut spoof_config = ContextConfig::default();
    spoof_config.spoof_os = "linux".to_string();
    spoof_config.spoof_cwd = "/srv/isolated_app".to_string();
    spoof_config.spoof_user_home = "/home/developer".to_string();
    spoof_config.spoof_datetime = "Monday, Jan 1, 2024, 00:00:00 UTC".to_string();

    let spoofed_prompt = build_configured_system_prompt(&spoof_config);
    let passes_prompt = spoofed_prompt.contains("OS: linux")
        && spoofed_prompt.contains("/srv/isolated_app")
        && spoofed_prompt.contains("/home/developer")
        && spoofed_prompt.contains("Monday, Jan 1, 2024");

    let disabled_config = ContextConfig {
        enable_system_prompt: false,
        ..Default::default()
    };
    let passes_disabled = build_configured_system_prompt(&disabled_config).is_empty();

    let raw_tool_log = format!(
        "User directory: {}\nFile at {}/project.txt",
        std::env::var("USERPROFILE").unwrap_or_default(),
        std::env::var("USERPROFILE").unwrap_or_default()
    );
    let sanitized_log = mask_output_data(&raw_tool_log, &spoof_config);
    let passes_mask = !sanitized_log.contains(&std::env::var("USERPROFILE").unwrap_or_default())
        && sanitized_log.contains("/home/developer");

    if passes_prompt && passes_disabled && passes_mask {
        println!("OK! Prompt spoofing, total disabling, and path sanitizing verified.");
    } else {
        println!(
            "FAIL! (passes_prompt={}, passes_disabled={}, passes_mask={})",
            passes_prompt, passes_disabled, passes_mask
        );
    }

    println!("==================================================");
    println!("  Diagnostics Completed. Ready for Headless Run.  ");
    println!("==================================================");
}

pub async fn run_agent_session(
    query: &str,
    is_deep: bool,
    model_override: Option<&str>,
    verbose: bool,
) {
    let base_url = "http://127.0.0.1:1234";

    // Detect model
    let model = if let Some(m) = model_override {
        m.to_string()
    } else {
        let models = fetch_available_models(base_url, None).await;
        if let Some(loaded) = models.iter().find(|m| m.is_loaded) {
            loaded.id.clone()
        } else if let Some(first) = models.first() {
            first.id.clone()
        } else {
            "huihui-qwen3.8-27b-abliterated".to_string()
        }
    };

    println!("==================================================");
    println!("  Silo Headless Session");
    println!(
        "  Mode:  {}",
        if is_deep {
            "🔬 DEEP RESEARCH"
        } else {
            "💬 STANDARD CHAT"
        }
    );
    println!("  Model: {}", model);
    println!("  Query: {}", query);
    println!("==================================================");

    let mut messages: Vec<ChatMessage> = Vec::new();
    let system_prompt = build_system_prompt();

    if is_deep {
        messages.push(ChatMessage::system(DEEP_RESEARCH_DIRECTIVE));
    }
    messages.push(ChatMessage::user(query));

    let max_tool_steps = if is_deep { 25 } else { 6 };
    let max_fetches = if is_deep { 10 } else { 2 };
    let mut fetch_count = 0;
    let mut step = 0;

    while step < max_tool_steps {
        step += 1;
        println!(
            "\n--- [Step {}/{}] Generating completion from model... ---",
            step, max_tool_steps
        );

        let (tx, mut rx) = unbounded_channel();
        let cancel_token = CancellationToken::new();

        let model_clone = model.clone();
        let sys_prompt_clone = system_prompt.clone();
        let msgs_clone = messages.clone();

        let enable_tools = !(is_deep && fetch_count >= max_fetches);

        let forced_tool = if step == 1 {
            let lower_q = query.to_lowercase();
            if lower_q.starts_with("/calc") {
                Some("execute_python".to_string())
            } else if lower_q.starts_with("/run") {
                Some("run_command".to_string())
            } else {
                None
            }
        } else {
            None
        };

        tokio::spawn(async move {
            stream_completion(
                base_url,
                None,
                &model_clone,
                &sys_prompt_clone,
                &msgs_clone,
                enable_tools,
                forced_tool,
                tx,
                cancel_token,
            )
            .await;
        });

        let mut live_content = String::new();
        let mut live_thinking = String::new();
        let mut final_content = String::new();
        let mut final_thinking = None;
        let mut tool_calls = Vec::new();
        let mut had_error = None;

        while let Some(ev) = rx.recv().await {
            match ev {
                StreamUpdate::Token(t) => {
                    if live_content.is_empty() && t.trim().is_empty() {
                        // Skip leading whitespace/newlines
                    } else {
                        print!("{}", t);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        live_content.push_str(&t);
                    }
                }
                StreamUpdate::Thinking(th) => {
                    live_thinking.push_str(&th);
                }
                StreamUpdate::ToolDrafting(_) => {}
                StreamUpdate::Payload(_) => {}
                StreamUpdate::Log(l) => {
                    if verbose {
                        eprintln!("[LOG] {}", l);
                    }
                }
                StreamUpdate::Finished {
                    full_content,
                    full_thinking,
                    tool_calls: tc,
                } => {
                    final_content = full_content;
                    final_thinking = full_thinking;
                    tool_calls = tc;
                    break;
                }
                StreamUpdate::Error(err) => {
                    had_error = Some(err);
                    break;
                }
            }
        }

        if let Some(err) = had_error {
            eprintln!("\n⚠️ Generation error: {}", err);
            break;
        }

        let content = if !final_content.is_empty() {
            final_content
        } else {
            live_content
        };
        let thinking = final_thinking.or(if !live_thinking.is_empty() {
            Some(live_thinking)
        } else {
            None
        });

        if let Some(ref th) = thinking {
            if verbose {
                println!(
                    "\n[Thinking ({} chars)]:\n{}\n--- End Thinking ---",
                    th.len(),
                    th
                );
            } else {
                println!("\n[Thinking completed: {} chars]", th.len());
            }
        }

        // If the model already produced a complete dossier with sources, do not loop on trailing citations
        if content.len() > 800
            && (content.contains("## Sources")
                || content.contains("### Sources")
                || content.contains("## Источники")
                || content.contains("### Источники"))
        {
            tool_calls.clear();
        }

        // If no tool calls, check for transitional pause or finalize
        if tool_calls.is_empty() {
            let lower_c = content.to_lowercase();
            let is_transitional = is_deep
                && content.len() < 400
                && step < max_tool_steps
                && (lower_c.contains("fetch")
                    || lower_c.contains("search")
                    || lower_c.contains("pull")
                    || lower_c.contains("read")
                    || lower_c.contains("source")
                    || lower_c.contains("let me")
                    || lower_c.contains("ищу")
                    || lower_c.contains("найду")
                    || lower_c.contains("поищу")
                    || lower_c.contains("запускаю")
                    || lower_c.contains("разведк"));

            if is_transitional {
                println!("\n[Agentic Nudge]: Model announced transitional intent ({} chars). Nudging for tool delegation...", content.len());
                messages.push(ChatMessage::assistant(&content, thinking));
                messages.push(ChatMessage::user("[DEEP RESEARCH INSTRUCTION]: You indicated you need to search or inspect sources. Call `web_search(query)` or `fetch_page(url)` now, or deliver the full final answer."));
                continue;
            }

            if content.trim().is_empty() {
                if let Some(th) = &thinking {
                    if !th.trim().is_empty() {
                        println!(
                            "\n[Notice]: Model completed thinking without emitting markdown text."
                        );
                        messages.push(ChatMessage::assistant("*(Completed reasoning)*", thinking));
                        break;
                    }
                }
                break;
            }

            messages.push(ChatMessage::assistant(&content, thinking));
            println!("\n=== Final Response Completed ===");
            break;
        }

        // Process tool calls
        println!("\n⚡ Model issued {} tool call(s):", tool_calls.len());
        let mut assistant_records = Vec::new();
        for tc in &tool_calls {
            println!("  • Tool: {} | Args: {}", tc.name, tc.arguments);
            assistant_records.push(ToolCallRecord {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                output: None,
                status: ToolStatus::Running,
            });
        }

        let mut assistant_msg = ChatMessage::assistant(&content, thinking);
        assistant_msg.tool_calls = assistant_records.clone();
        messages.push(assistant_msg);

        // Execute each tool call
        for tc in &tool_calls {
            println!("\nExecuting tool [{}]...", tc.name);
            let (ok, out) = execute_tool(&tc.name, &tc.arguments).await;
            println!(
                "Tool [{}] -> {}: ({} chars output)",
                tc.name,
                if ok { "SUCCESS" } else { "FAILED" },
                out.len()
            );

            let instruction = if tc.name == "web_search" || tc.name == "search" {
                if is_deep && fetch_count < max_fetches {
                    format!("Results:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Review these results. Select the most authoritative URLs and call `fetch_page(url)` to read their full content. You have {} page reads remaining.", out, max_fetches - fetch_count)
                } else {
                    format!(
                        "Results:\n{}\n\n[INSTRUCTION]: Synthesize your final answer now.",
                        out
                    )
                }
            } else if tc.name == "fetch_page" || tc.name == "fetch" {
                fetch_count += 1;
                if is_deep && fetch_count < max_fetches {
                    format!("Page Markdown:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Source read [{}/{}]. If additional pages or searches are needed to complete the research, proceed. If sufficient evidence has been gathered, produce the final comprehensive dossier.", out, fetch_count, max_fetches)
                } else {
                    format!("Page Markdown:\n{}\n\n[INSTRUCTION]: Now synthesize your final comprehensive dossier.", out)
                }
            } else if tc.name == "execute_python" || tc.name == "python" {
                if is_deep {
                    format!("Python Execution Result:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Computation complete. Incorporate these exact numbers into your analysis. If additional searches or pages are needed, proceed; otherwise, synthesize your comprehensive dossier.", out)
                } else {
                    format!("Python Execution Result:\n{}\n\n[INSTRUCTION]: Use this exact numerical result to formulate your final response to the user.", out)
                }
            } else {
                format!("Command Execution Result:\n{}\n\n[INSTRUCTION]: Inspect stdout/stderr. Proceed with the task.", out)
            };

            messages.push(ChatMessage::tool(instruction, &tc.id));
        }
    }
}
