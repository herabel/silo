use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

pub fn find_uv_bin() -> PathBuf {
    // 1. Direct system PATH search
    if let Ok(paths) = std::env::var("PATH") {
        for p in std::env::split_paths(&paths) {
            #[cfg(windows)]
            let candidate = p.join("uv.exe");
            #[cfg(not(windows))]
            let candidate = p.join("uv");

            if candidate.is_file() {
                return candidate;
            }
        }
    }

    // 2. Standard user directories on Windows
    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local_app_data);
            let candidates = [
                base.join(r"Programs\uv\uv.exe"),
                base.join(r"uv\uv.exe"),
            ];
            for c in candidates {
                if c.is_file() {
                    return c;
                }
            }
            // Dynamically scan any Python scripts directory in LOCALAPPDATA without hardcoding versions
            let py_dir = base.join(r"Programs\Python");
            if let Ok(entries) = std::fs::read_dir(py_dir) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join(r"Scripts\uv.exe");
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
            let py_core_dir = base.join("Python");
            if let Ok(entries) = std::fs::read_dir(py_core_dir) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join(r"Scripts\uv.exe");
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }

        if let Ok(user_profile) = std::env::var("USERPROFILE") {
            let base = PathBuf::from(user_profile);
            let candidate = base.join(r".cargo\bin\uv.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    PathBuf::from("uv")
}

pub fn get_skills_dir() -> PathBuf {
    // 1. Check next to current executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let candidate = parent.join("skills");
            if candidate.is_dir() {
                return candidate;
            }
            // Check if running from target/release or target/debug
            if let Some(repo_root) = parent.parent().and_then(|p| p.parent()) {
                let candidate = repo_root.join("skills");
                if candidate.is_dir() {
                    return candidate;
                }
            }
        }
    }
    // 2. Check in current working directory
    let cwd_candidate = PathBuf::from("skills");
    if cwd_candidate.is_dir() {
        return cwd_candidate;
    }
    // 3. User config directory fallback
    #[cfg(windows)]
    if let Ok(app_data) = std::env::var("APPDATA") {
        let candidate = PathBuf::from(app_data).join("silo").join("skills");
        if candidate.is_dir() {
            return candidate;
        }
    }
    PathBuf::from("skills")
}

pub fn list_installed_skills() -> Vec<SkillInfo> {
    let skills_dir = get_skills_dir();
    let mut results = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.is_file() {
                    let folder_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let description = parse_skill_description(&skill_md)
                        .unwrap_or_else(|| format!("Skill: {}", folder_name));

                    results.push(SkillInfo {
                        name: folder_name,
                        description,
                    });
                }
            }
        }
    }

    results
}

fn parse_skill_description(skill_md_path: &Path) -> Option<String> {
    if let Ok(content) = std::fs::read_to_string(skill_md_path) {
        let mut in_frontmatter = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "---" {
                if in_frontmatter {
                    break;
                } else {
                    in_frontmatter = true;
                    continue;
                }
            }
            if in_frontmatter && trimmed.starts_with("description:") {
                let desc = trimmed.trim_start_matches("description:").trim();
                let clean = desc.trim_matches('"').trim_matches('\'').trim();
                return Some(clean.to_string());
            }
        }
    }
    None
}

pub async fn run_skill_script(skill_name: &str, script: &str, args: &[String]) -> (bool, String) {
    let skills_dir = get_skills_dir();
    let skill_path = skills_dir.join(skill_name);

    if !skill_path.is_dir() {
        return (
            false,
            format!(
                "Error: Skill '{}' not found in '{}'",
                skill_name,
                skills_dir.display()
            ),
        );
    }

    let abs_skill_path = match std::fs::canonicalize(&skill_path) {
        Ok(p) => p,
        Err(_) => skill_path.clone(),
    };

    // Clean script path (prevent directory traversal out of skill dir)
    let clean_script = script.trim().replace('\\', "/");
    if clean_script.contains("..") {
        return (false, "Error: Script path must not contain '..'".to_string());
    }

    // Check if we need to provide an ephemeral in-RAM image handle to the script
    let mut resolved_args: Vec<String> = args.to_vec();
    let _ephem_guard = if let Some(img_bytes) = crate::engine::get_active_session_image() {
        let needs_image = clean_script.contains("intake")
            || clean_script.contains("board")
            || clean_script.contains("ocr")
            || clean_script.contains("match")
            || clean_script.contains("exif")
            || args.iter().any(|a| a.contains("photo") || a.contains("image") || a.ends_with(".png") || a.ends_with(".jpg"));

        if needs_image {
            match crate::engine::EphemeralMemoryFile::create_from_bytes(&img_bytes, "png") {
                Ok(guard) => {
                    let ephem_path_str = guard.path.to_string_lossy().to_string();
                    let mut replaced = false;
                    for arg in &mut resolved_args {
                        if arg.contains("photo") || arg.contains("image") || arg.ends_with(".png") || arg.ends_with(".jpg") {
                            *arg = ephem_path_str.clone();
                            replaced = true;
                        }
                    }
                    if !replaced && (clean_script.contains("intake") || clean_script.contains("ocr") || clean_script.contains("exif")) {
                        resolved_args.insert(0, ephem_path_str);
                    }
                    Some(guard)
                }
                Err(e) => {
                    eprintln!("Warning: Failed to create in-RAM ephemeral file: {}", e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // If an intake script requires --out-dir, ensure a default output directory is provided if omitted
    if clean_script.contains("intake") && !resolved_args.iter().any(|a| a == "--out-dir" || a.starts_with("--out-dir=")) {
        resolved_args.push("--out-dir".to_string());
        resolved_args.push("intake".to_string());
    }

    let uv_bin = find_uv_bin();
    let mut cmd = Command::new(&uv_bin);
    cmd.arg("run");

    // Automatically resolve script in skill root or scripts/ subfolder
    let effective_script = if abs_skill_path.join(&clean_script).exists() {
        clean_script.clone()
    } else if abs_skill_path.join("scripts").join(&clean_script).exists() {
        format!("scripts/{}", clean_script)
    } else {
        clean_script.clone()
    };
    cmd.arg(&effective_script);
    for arg in &resolved_args {
        cmd.arg(arg);
    }

    cmd.current_dir(&abs_skill_path);
    cmd.env("SKILL_DIR", abs_skill_path.to_string_lossy().as_ref());
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env("PYTHONUTF8", "1");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW
        cmd.creation_flags(0x08000000);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                format!(
                    "Failed to spawn '{} run {}': {}. Is 'uv' installed?",
                    uv_bin.display(), clean_script, e
                ),
            );
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let timeout_duration = Duration::from_secs(180);
    let result = timeout(timeout_duration, child.wait()).await;

    match result {
        Ok(Ok(status)) => {
            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();
            if let Some(mut out) = stdout {
                let _ = out.read_to_end(&mut out_buf).await;
            }
            if let Some(mut err) = stderr {
                let _ = err.read_to_end(&mut err_buf).await;
            }

            let stdout_str = String::from_utf8_lossy(&out_buf).to_string();
            let stderr_str = String::from_utf8_lossy(&err_buf).to_string();

            if status.success() {
                let mut combined = if stderr_str.trim().is_empty() {
                    stdout_str
                } else if stdout_str.trim().is_empty() {
                    stderr_str
                } else {
                    format!("{}\n[stderr]:\n{}", stdout_str, stderr_str)
                };

                // Check if skill referenced a generated report (e.g. -> intake\intake.md (read this))
                if let Some(pos) = combined.find("-> ") {
                    let after_arrow = &combined[pos + 3..];
                    let file_token = after_arrow.split_whitespace().next().unwrap_or("").trim();
                    let clean_token = file_token.replace('\\', "/");
                    let report_path = abs_skill_path.join(&clean_token);
                    if report_path.exists() && report_path.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&report_path) {
                            combined.push_str("\n\n---\n[Skill Generated Report (");
                            combined.push_str(&clean_token);
                            combined.push_str(")]:\n");
                            if content.chars().count() > 4000 {
                                combined.push_str(&content.chars().take(3800).collect::<String>());
                                combined.push_str("\n...[Report truncated for brevity]...");
                            } else {
                                combined.push_str(&content);
                            }
                        }
                    }
                }

                (true, combined)
            } else {
                let code = status.code().unwrap_or(-1);
                let combined = format!(
                    "Script exited with status code {}:\n{}\n{}",
                    code, stdout_str, stderr_str
                );
                (false, combined)
            }
        }
        Ok(Err(e)) => (false, format!("Failed to wait for script execution: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            (
                false,
                format!(
                    "Timeout: Script execution exceeded {} seconds",
                    timeout_duration.as_secs()
                ),
            )
        }
    }
}
