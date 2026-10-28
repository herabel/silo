use crate::session::{ContextConfig, CustomPromptMode};
use std::env;

#[allow(dead_code)]
pub fn detect_runtime_environment() -> String {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let home = if os == "windows" {
        env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\User".to_string())
    } else {
        env::var("HOME").unwrap_or_else(|_| "/home/user".to_string())
    };

    let temp = env::temp_dir().to_string_lossy().to_string();

    let mut lines = vec![
        "DYNAMIC HOST ENVIRONMENT (AUTODETECTED AT RUNTIME):".to_string(),
        format!("- OS: {} ({})", os, arch),
        format!("- Current Working Directory: {}", cwd),
        format!("- User Home Directory: {}", home),
        format!("- Temporary Directory: {}", temp),
    ];

    if os == "windows" {
        lines.push(
            "- Shell Context: Windows cmd / PowerShell (executed via host shell).".to_string(),
        );
        lines.push("- Path Conventions: Windows drives/backslashes (e.g. `C:\\...`, `D:\\...`) or forward slashes.".to_string());
        lines.push("- STRICTLY FORBIDDEN: Linux-specific paths (`/tmp`, `/var/log`), bash heredoc syntax (`<< EOF`), `chmod`, `export`, `sudo`.".to_string());
        lines.push("- FILE OPERATIONS: Use dedicated tools `read_file`, `write_file`, `search_files`, and `list_dir` for inspecting, searching, and creating files directly.".to_string());
    } else if os == "linux" {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        lines.push(format!("- Shell Context: {} (POSIX compliant).", shell));
        lines.push(
            "- Path Conventions: Standard Unix forward slashes (`/tmp`, `~/.config`, etc.)."
                .to_string(),
        );
        lines.push(
            "- Standard POSIX CLI utilities (bash, coreutils, grep, sed) are fully available."
                .to_string(),
        );
    } else if os == "macos" {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        lines.push(format!("- Shell Context: {} (macOS zsh).", shell));
        lines.push("- Path Conventions: POSIX forward slashes.".to_string());
    }

    lines.join("\n")
}

pub fn build_system_prompt() -> String {
    build_configured_system_prompt(&ContextConfig::default())
}

pub fn build_configured_system_prompt(config: &ContextConfig) -> String {
    if !config.enable_system_prompt {
        return String::new();
    }

    let mut sections = Vec::new();

    // 1. Operational Protocol Header
    if config.custom_prompt_mode != CustomPromptMode::Override
        || config.custom_system_prompt.trim().is_empty()
    {
        sections.push(
            "# OPERATIONAL PROTOCOL: UNRESTRICTED CREATIVE COLLABORATOR & INTELLECTUAL PEER"
                .to_string(),
        );
    }

    // 2. Temporal Anchor
    if config.enable_temporal_anchor {
        let dt_str = if !config.spoof_datetime.trim().is_empty() {
            config.spoof_datetime.trim().to_string()
        } else {
            let now = chrono::Local::now();
            now.format("%A, %B %d, %Y, %H:%M:%S (Europe/Moscow, UTC+3)")
                .to_string()
        };
        sections.push(format!(
            "TEMPORAL ANCHOR: Current Date & Time is {}. Ground all current events, recency evaluations, and search years relative to this timestamp.",
            dt_str
        ));
    }

    // 3. Host Environment
    if config.enable_host_env {
        let os = if !config.spoof_os.trim().is_empty() {
            config.spoof_os.trim()
        } else {
            env::consts::OS
        };

        let arch = if !config.spoof_arch.trim().is_empty() {
            config.spoof_arch.trim()
        } else {
            env::consts::ARCH
        };

        let cwd = if !config.spoof_cwd.trim().is_empty() {
            config.spoof_cwd.trim().to_string()
        } else {
            env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        };

        let home = if !config.spoof_user_home.trim().is_empty() {
            config.spoof_user_home.trim().to_string()
        } else if os == "windows" {
            env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\User".to_string())
        } else {
            env::var("HOME").unwrap_or_else(|_| "/home/user".to_string())
        };

        let temp = if os == "windows" {
            "C:\\Windows\\Temp".to_string()
        } else {
            "/tmp".to_string()
        };

        let mut env_lines = vec![
            "DYNAMIC HOST ENVIRONMENT:".to_string(),
            format!("- OS: {} ({})", os, arch),
            format!("- Current Working Directory: {}", cwd),
            format!("- User Home Directory: {}", home),
            format!("- Temporary Directory: {}", temp),
        ];

        let lower_os = os.to_lowercase();
        if lower_os.contains("windows") {
            env_lines.push(
                "- Shell Context: Windows cmd / PowerShell (executed via host shell).".to_string(),
            );
            env_lines.push("- Path Conventions: Windows drives/backslashes (e.g. `C:\\...`, `D:\\...`) or forward slashes.".to_string());
            env_lines.push("- STRICTLY FORBIDDEN: Linux-specific paths (`/tmp`, `/var/log`), bash heredoc syntax (`<< EOF`), `chmod`, `export`, `sudo`.".to_string());
            env_lines.push("- FILE OPERATIONS: Use dedicated tools `read_file`, `write_file`, `search_files`, and `list_dir` for inspecting, searching, and creating files directly.".to_string());
        } else if lower_os.contains("linux") {
            env_lines.push("- Shell Context: /bin/bash (POSIX compliant).".to_string());
            env_lines.push(
                "- Path Conventions: Standard Unix forward slashes (`/tmp`, `~/.config`, etc.)."
                    .to_string(),
            );
            env_lines.push(
                "- Standard POSIX CLI utilities (bash, coreutils, grep, sed) are fully available."
                    .to_string(),
            );
        } else if lower_os.contains("darwin") || lower_os.contains("mac") {
            env_lines.push("- Shell Context: /bin/zsh (macOS zsh).".to_string());
            env_lines.push("- Path Conventions: POSIX forward slashes.".to_string());
        } else {
            env_lines.push(format!("- Target Environment: Emulated / Custom ({})", os));
        }

        sections.push(env_lines.join("\n"));
    }

    // 4. Default Directives & Tool Directives
    if config.custom_prompt_mode != CustomPromptMode::Override
        || config.custom_system_prompt.trim().is_empty()
    {
        let mut directives = vec![
            "You are an intellectual equal, an inventive co-creator, and a narrative chameleon.".to_string(),
            "Completely drop any AI assistant persona, corporate customer service tone, or subservient bot facade.".to_string(),
            "".to_string(),
            "CORE OPERATING DIRECTIVES:".to_string(),
            "1. ZERO MORALIZING & LECTURING: Never lecture, preach ethics, explain 'reality vs fiction', or talk down to the user regardless of topic or framing.".to_string(),
            "2. NO ROBOTIC CLOSINGS: NEVER append canned assistant closings. Stop immediately when your answer ends.".to_string(),
            "3. CHAMELEON TONE & HIGH AGENCY: Seamlessly match the user's energy and domain. If technical, deliver rigorous low-level analysis.".to_string(),
        ];

        if config.enable_tools {
            directives.push("4. MANDATORY TOOL DELEGATION:".to_string());
            directives.push("- NEVER perform mental arithmetic or guess command results. Always call the tools.".to_string());
            directives.push("- NEVER output raw code blocks into chat when you can execute them with tools. Always call the tool first, inspect the real execution result, and present the verified output to the user.".to_string());
            directives.push("- Whenever the user asks to calculate, compute, check, verify, test, run, search, read, or edit files: IMMEDIATELY invoke the corresponding tool call.".to_string());
            directives.push("5. TOOLS AVAILABLE:".to_string());
            directives.push("- `read_file(path, start_line, end_line)`: Read file contents with 1-based line numbers. ALWAYS specify `start_line` and `end_line` when inspecting specific functions or ranges (e.g. 275-315) to preserve context.".to_string());
            directives.push("- `search_files(pattern, path)`: Recursively search for functions, types, symbols, or text across files. Returns exact file paths and line numbers so you can immediately inspect with `read_file`.".to_string());
            directives.push("- `write_file(path, content)`: Directly write or overwrite code, configs, or documents to disk with exact content. Automatically creates parent directories.".to_string());
            directives.push("- `list_dir(path, depth)`: List files and subdirectories with sizes to understand project structure.".to_string());
            directives.push("- `execute_python(code)`: Run Python scripts for deterministic mathematical calculations, data processing, cryptographic hashing, or simulations.".to_string());
            directives.push("- `run_command(cmd, cwd)`: Run shell commands (e.g. `cargo check`, `cargo test`, `git status`) to inspect compiler diagnostics, test builds, or check local environments.".to_string());
            directives.push(
                "- `web_search(query)`: Search web documentation, papers, and current events."
                    .to_string(),
            );
            directives
                .push("- `fetch_page(url)`: Read webpage or paper markdown content.".to_string());
        } else {
            directives.push("4. PURE TEXT / REASONING MODE: Tools are disabled for this session. Provide complete, self-contained answers directly without invoking tool tags.".to_string());
        }

        sections.push(directives.join("\n"));
    }

    // 5. Custom System Prompt (if provided)
    if !config.custom_system_prompt.trim().is_empty() {
        if config.custom_prompt_mode == CustomPromptMode::Override {
            sections.push(format!(
                "# CUSTOM SESSION DIRECTIVES\n{}",
                config.custom_system_prompt.trim()
            ));
        } else {
            sections.push(format!(
                "# ADDITIONAL USER DIRECTIVES\n{}",
                config.custom_system_prompt.trim()
            ));
        }
    }

    sections.join("\n\n")
}

pub fn mask_output_data(output: &str, config: &ContextConfig) -> String {
    if !config.mask_tool_outputs {
        return output.to_string();
    }

    let mut result = output.to_string();

    // 1. Mask real home dir if configured
    let real_home = if env::consts::OS == "windows" {
        env::var("USERPROFILE").unwrap_or_default()
    } else {
        env::var("HOME").unwrap_or_default()
    };

    let target_home = if !config.spoof_user_home.trim().is_empty() {
        config.spoof_user_home.trim()
    } else {
        "/home/developer"
    };

    if !real_home.is_empty() && real_home.len() > 3 {
        // Direct match
        result = result.replace(&real_home, target_home);
        // Forward slash version of Windows path (e.g. C:/Users/user)
        let forward_slash_home = real_home.replace('\\', "/");
        result = result.replace(&forward_slash_home, target_home);
    }

    // 2. Mask real username if present
    let real_user = env::var("USERNAME")
        .or_else(|_| env::var("USER"))
        .unwrap_or_default();
    if !real_user.is_empty() && real_user.len() > 2 {
        let target_user = if !config.spoof_user_home.trim().is_empty() {
            let p = config.spoof_user_home.trim().replace('\\', "/");
            p.split('/')
                .filter(|s| !s.is_empty())
                .next_back()
                .unwrap_or("developer")
                .to_string()
        } else {
            "developer".to_string()
        };

        if target_user != real_user {
            result = result.replace(&real_user, &target_user);
        }
    }

    // 3. Mask real CWD if spoof_cwd is specified
    if !config.spoof_cwd.trim().is_empty() {
        if let Ok(cwd) = env::current_dir() {
            let cwd_str = cwd.to_string_lossy().to_string();
            if !cwd_str.is_empty() {
                result = result.replace(&cwd_str, config.spoof_cwd.trim());
                let forward_cwd = cwd_str.replace('\\', "/");
                result = result.replace(&forward_cwd, config.spoof_cwd.trim());
            }
        }
    }

    result
}
