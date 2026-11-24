use crate::engine::execute_tool;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run_mcp_server() {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    eprintln!("[Silo MCP] Server started on stdio. Waiting for JSON-RPC messages...");

    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[Silo MCP] JSON parse error: {}", e);
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }
                });
                send_response(&mut stdout, &err_resp).await;
                continue;
            }
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        eprintln!("[Silo MCP] Received method: {}", method);

        match method {
            "initialize" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "silo-mcp",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                });
                send_response(&mut stdout, &resp).await;
            }
            "notifications/initialized" => {
                eprintln!("[Silo MCP] Client confirmed initialization.");
            }
            "ping" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                });
                send_response(&mut stdout, &resp).await;
            }
            "tools/list" => {
                let tools = get_mcp_tools();
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": tools
                    }
                });
                send_response(&mut stdout, &resp).await;
            }
            "tools/call" => {
                let params = msg.get("params");
                let name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                eprintln!("[Silo MCP] Calling tool '{}' with args: {}", name, args);

                let result = handle_tool_call(name, args).await;
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": result
                            }
                        ]
                    }
                });
                send_response(&mut stdout, &resp).await;
            }
            _ => {
                eprintln!("[Silo MCP] Method not found: {}", method);
                if id.is_some() {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {}", method)
                        }
                    });
                    send_response(&mut stdout, &resp).await;
                }
            }
        }
    }

    eprintln!("[Silo MCP] Stdin closed. Server exiting.");
}

async fn send_response(stdout: &mut tokio::io::Stdout, val: &Value) {
    if let Ok(json_str) = serde_json::to_string(val) {
        let _ = stdout.write_all(json_str.as_bytes()).await;
        let _ = stdout.write_all(b"\n").await;
        let _ = stdout.flush().await;
    }
}

fn get_mcp_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "silo_run_command",
            "description": "Execute a shell command locally in the workspace (e.g. 'go test -bench . -benchmem', 'cargo check', 'git status')",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command line string to execute"
                    }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "silo_read_file",
            "description": "Read contents of a file from disk with UTF-8 decoding",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "silo_write_file",
            "description": "Write or overwrite a file on disk with the given content",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to create or overwrite"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write into the file"
                    }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "silo_search_files",
            "description": "Search for text/regex pattern across files in a directory",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Root directory path to search in (defaults to current dir)"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "silo_eval_python",
            "description": "Safely evaluate Python code locally for math, simulation, benchmarks, or data analysis",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Python code to execute"
                    }
                },
                "required": ["code"]
            }
        }),
        json!({
            "name": "silo_deep_research",
            "description": "Run an autonomous multi-step technical deep research investigation using local inference (OmniCoder / GLM / Qwen) via Silo Headless engine",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Topic or problem statement for exhaustive investigation"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model override (e.g. 'omnicoder-9b', 'glm-4.7-flash', 'huihui-qwen3.8-27b-abliterated')"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "silo_arxiv_search",
            "description": "Search academic research papers on arXiv by query",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords or topic to search on arXiv"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "silo_fetch_arxiv_paper",
            "description": "Fetch and ingest the full technical paper text from arXiv into the RAG Research Board using BM25 chunk ranking",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_id": {
                        "type": "string",
                        "description": "arXiv paper ID (e.g. '2401.03185') or URL"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional search query to rank and extract relevant sections via BM25"
                    }
                },
                "required": ["paper_id"]
            }
        }),
        json!({
            "name": "silo_web_search",
            "description": "Search the public web for technical documentation, issues, and advisories",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    }
                },
                "required": ["query"]
            }
        })
    ]
}

async fn handle_tool_call(name: &str, args: Value) -> String {
    match name {
        "silo_run_command" => {
            let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let args_json = json!({ "command": cmd }).to_string();
            let (_ok, out) = execute_tool("run_command", &args_json).await;
            out
        }
        "silo_read_file" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let args_json = json!({ "path": path }).to_string();
            let (_ok, out) = execute_tool("read_file", &args_json).await;
            out
        }
        "silo_write_file" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let args_json = json!({ "path": path, "content": content }).to_string();
            let (_ok, out) = execute_tool("write_file", &args_json).await;
            out
        }
        "silo_search_files" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
            let args_json = json!({ "query": query, "path": path }).to_string();
            let (_ok, out) = execute_tool("search_files", &args_json).await;
            out
        }
        "silo_eval_python" => {
            let code = args.get("code").and_then(|c| c.as_str()).unwrap_or("");
            let args_json = json!({ "code": code }).to_string();
            let (_ok, out) = execute_tool("execute_python", &args_json).await;
            out
        }
        "silo_arxiv_search" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let args_json = json!({ "query": query }).to_string();
            let (_ok, out) = execute_tool("arxiv_search", &args_json).await;
            out
        }
        "silo_fetch_arxiv_paper" => {
            let paper_id = args.get("paper_id").and_then(|p| p.as_str()).unwrap_or("");
            let query = args.get("query").and_then(|q| q.as_str());
            let args_json = json!({ "paper_id": paper_id, "query": query }).to_string();
            let (_ok, out) = execute_tool("fetch_arxiv_paper", &args_json).await;
            out
        }
        "silo_web_search" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let args_json = json!({ "query": query }).to_string();
            let (_ok, out) = execute_tool("web_search", &args_json).await;
            out
        }
        "silo_deep_research" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let model = args.get("model").and_then(|m| m.as_str()).map(|s| s.to_string());
            
            // Invoke headless deep research directly
            let mut headless_args = vec![
                "silo.exe".to_string(),
                "--headless".to_string(),
                "--deep".to_string(),
                query.to_string(),
            ];
            if let Some(m) = model {
                headless_args.push("--model".to_string());
                headless_args.push(m);
            }
            
            // Capture headless run output by running in quiet/mcp-safe mode
            let out = run_headless_capture(headless_args).await;
            out
        }
        _ => format!("Unknown Silo tool: {}", name),
    }
}

async fn run_headless_capture(args: Vec<String>) -> String {
    // Spawn self as subprocess to avoid blocking stdio or mixing stdout streams
    if let Ok(exe_path) = std::env::current_exe() {
        let mut cmd = tokio::process::Command::new(exe_path);
        // Skip argv[0]
        if args.len() > 1 {
            cmd.args(&args[1..]);
        }
        
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                if output.status.success() {
                    stdout
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    format!("[Research Error]: {}\n{}", stdout, stderr)
                }
            }
            Err(e) => format!("Failed to spawn headless research subprocess: {}", e),
        }
    } else {
        "Failed to determine current executable path".to_string()
    }
}
