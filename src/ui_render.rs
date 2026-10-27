use eframe::egui::{self, Color32, Frame, Margin, RichText, Rounding, Stroke};

#[derive(Debug, Clone)]
pub enum ContentSegment {
    Text(String),
    CodeBlock {
        language: String,
        code: String,
    },
}

#[derive(Debug, Clone)]
pub enum TextBlock {
    Heading { level: usize, text: String },
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    Divider,
    ListItem { bullet: String, text: String },
    Paragraph(String),
}

pub fn parse_markdown_segments(input: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut current_pos = 0;
    let bytes = input.as_bytes();
    let len = bytes.len();

    while current_pos < len {
        if let Some(fence_start) = input[current_pos..].find("```") {
            let actual_start = current_pos + fence_start;

            if actual_start > current_pos {
                let text_chunk = &input[current_pos..actual_start];
                if !text_chunk.is_empty() {
                    segments.push(ContentSegment::Text(text_chunk.to_string()));
                }
            }

            let after_fence = actual_start + 3;
            let (lang, code_start) = if let Some(nl_pos) = input[after_fence..].find('\n') {
                let lang_str = input[after_fence..after_fence + nl_pos].trim().to_string();
                (lang_str, after_fence + nl_pos + 1)
            } else {
                ("".to_string(), after_fence)
            };

            if let Some(fence_end) = input[code_start..].find("```") {
                let code_content = input[code_start..code_start + fence_end].trim_end_matches('\n');
                segments.push(ContentSegment::CodeBlock {
                    language: if lang.is_empty() { "code".to_string() } else { lang },
                    code: code_content.to_string(),
                });
                current_pos = code_start + fence_end + 3;
            } else {
                let code_content = input[code_start..].trim_end_matches('\n');
                segments.push(ContentSegment::CodeBlock {
                    language: if lang.is_empty() { "code".to_string() } else { lang },
                    code: code_content.to_string(),
                });
                break;
            }
        } else {
            let remaining = &input[current_pos..];
            if !remaining.is_empty() {
                segments.push(ContentSegment::Text(remaining.to_string()));
            }
            break;
        }
    }

    segments
}

pub fn append_rich_spans(
    job: &mut egui::text::LayoutJob,
    text: &str,
    base_size: f32,
    base_color: Color32,
    force_bold: bool,
    force_italic: bool,
) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    let mut current_text = String::new();
    let mut is_bold = force_bold;
    let mut is_italic = force_italic;
    let mut is_code = false;

    let flush = |job: &mut egui::text::LayoutJob, txt: &mut String, bold: bool, italic: bool, code: bool| {
        if txt.is_empty() {
            return;
        }

        let font_id = if code {
            egui::FontId::new(base_size - 1.0, egui::FontFamily::Monospace)
        } else if bold {
            egui::FontId::new(base_size, egui::FontFamily::Name("bold".into()))
        } else {
            egui::FontId::new(base_size, egui::FontFamily::Proportional)
        };

        let color = if code {
            Color32::from_rgb(56, 189, 248)
        } else if bold {
            Color32::from_rgb(255, 255, 255)
        } else if italic {
            Color32::from_rgb(203, 213, 225)
        } else {
            base_color
        };

        let background = if code {
            Color32::from_rgb(28, 36, 48)
        } else {
            Color32::TRANSPARENT
        };

        job.append(
            txt,
            0.0,
            egui::TextFormat {
                font_id,
                color,
                background,
                italics: italic,
                ..Default::default()
            },
        );
        txt.clear();
    };

    while i < len {
        // 1. Inline code: `...`
        if chars[i] == '`' {
            flush(job, &mut current_text, is_bold, is_italic, is_code);
            is_code = !is_code;
            i += 1;
            continue;
        }

        if !is_code {
            // 2. Bold: **...** or __...__
            if (chars[i] == '*' && i + 1 < len && chars[i + 1] == '*')
                || (chars[i] == '_' && i + 1 < len && chars[i + 1] == '_')
            {
                flush(job, &mut current_text, is_bold, is_italic, is_code);
                is_bold = !is_bold;
                i += 2;
                continue;
            }

            // 3. Italic: *...* or _..._
            if chars[i] == '*' || chars[i] == '_' {
                flush(job, &mut current_text, is_bold, is_italic, is_code);
                is_italic = !is_italic;
                i += 1;
                continue;
            }
        }

        current_text.push(chars[i]);
        i += 1;
    }

    flush(job, &mut current_text, is_bold, is_italic, is_code);
}

pub fn create_rich_layout(
    text: &str,
    base_size: f32,
    base_color: Color32,
    force_bold: bool,
    force_italic: bool,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if wrap_width > 0.0 && wrap_width.is_finite() {
        job.wrap.max_width = wrap_width;
    }
    job.wrap.break_anywhere = false;
    append_rich_spans(&mut job, text, base_size, base_color, force_bold, force_italic);
    job
}

pub fn compute_column_widths(
    headers: &[String],
    rows: &[Vec<String>],
    total_available_width: f32,
    spacing: f32,
) -> Vec<f32> {
    let num_cols = headers.len();
    if num_cols == 0 {
        return Vec::new();
    }

    // Usable width inside frame (accounting for padding and inter-column spacing)
    let total_spacing = spacing * (num_cols as f32 - 1.0) + 24.0;
    let net_width = (total_available_width - total_spacing).max(num_cols as f32 * 110.0);

    // Calculate content length weight for each column
    let mut weights = Vec::with_capacity(num_cols);
    for col_idx in 0..num_cols {
        let mut max_len = headers[col_idx].chars().count();
        let mut total_chars = max_len;
        let mut row_count = 1;

        for row in rows {
            if let Some(cell) = row.get(col_idx) {
                let cell_len = cell.chars().count();
                if cell_len > max_len {
                    max_len = cell_len;
                }
                total_chars += cell_len;
                row_count += 1;
            }
        }

        let avg_len = total_chars / row_count;
        let effective = (max_len as f32 * 0.6 + avg_len as f32 * 0.4).clamp(12.0, 65.0);
        weights.push(effective);
    }

    let sum_weights: f32 = weights.iter().sum();
    let min_col_width = 110.0_f32;

    if sum_weights <= 0.0 {
        let uniform = (net_width / num_cols as f32).max(min_col_width);
        return vec![uniform; num_cols];
    }

    weights
        .into_iter()
        .map(|w| ((w / sum_weights) * net_width).max(min_col_width))
        .collect()
}

pub fn parse_text_blocks(raw_text: &str) -> Vec<TextBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = raw_text.lines().collect();
    let num_lines = lines.len();
    let mut i = 0;

    while i < num_lines {
        let line = lines[i].trim();

        if line.is_empty() {
            i += 1;
            continue;
        }

        // Table detection: line starts and ends with '|'
        if line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2 {
            let mut table_lines = Vec::new();
            while i < num_lines && lines[i].trim().starts_with('|') && lines[i].trim().ends_with('|') {
                table_lines.push(lines[i].trim());
                i += 1;
            }

            if table_lines.len() >= 2 {
                let parse_cells = |row: &str| -> Vec<String> {
                    let trimmed = row.trim_matches('|');
                    trimmed.split('|').map(|c| c.trim().to_string()).collect()
                };

                let headers = parse_cells(table_lines[0]);
                let is_sep = |row: &str| -> bool {
                    row.chars().all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
                };

                let start_idx = if is_sep(table_lines[1]) { 2 } else { 1 };
                let mut rows = Vec::new();
                for row_str in &table_lines[start_idx..] {
                    let mut row_cells = parse_cells(row_str);
                    if row_cells.len() < headers.len() {
                        row_cells.resize(headers.len(), String::new());
                    } else if row_cells.len() > headers.len() {
                        row_cells.truncate(headers.len());
                    }
                    rows.push(row_cells);
                }

                blocks.push(TextBlock::Table { headers, rows });
                continue;
            }
        }

        // Horizontal Rule
        if line == "---" || line == "***" || line == "___" || line == "- - -" || line == "* * *" {
            blocks.push(TextBlock::Divider);
            i += 1;
            continue;
        }

        // Headings
        if line.starts_with("# ") {
            blocks.push(TextBlock::Heading { level: 1, text: line[2..].trim().to_string() });
            i += 1;
            continue;
        } else if line.starts_with("## ") {
            blocks.push(TextBlock::Heading { level: 2, text: line[3..].trim().to_string() });
            i += 1;
            continue;
        } else if line.starts_with("### ") {
            blocks.push(TextBlock::Heading { level: 3, text: line[4..].trim().to_string() });
            i += 1;
            continue;
        } else if line.starts_with("#### ") {
            blocks.push(TextBlock::Heading { level: 4, text: line[5..].trim().to_string() });
            i += 1;
            continue;
        }

        // Bullet lists
        if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
            let bullet = "•".to_string();
            let text = line[2..].trim().to_string();
            blocks.push(TextBlock::ListItem { bullet, text });
            i += 1;
            continue;
        }

        // Numbered lists
        if let Some(dot_pos) = line.find(". ") {
            if dot_pos > 0 && dot_pos <= 3 && line[..dot_pos].chars().all(|c| c.is_ascii_digit()) {
                let bullet = format!("{}.", &line[..dot_pos]);
                let text = line[dot_pos + 2..].trim().to_string();
                blocks.push(TextBlock::ListItem { bullet, text });
                i += 1;
                continue;
            }
        }

        // Regular paragraph
        let mut para_lines = vec![line];
        i += 1;
        while i < num_lines {
            let next_line = lines[i].trim();
            if next_line.is_empty()
                || next_line.starts_with('#')
                || next_line.starts_with('|')
                || next_line.starts_with("- ")
                || next_line.starts_with("* ")
                || next_line == "---"
                || next_line == "***"
            {
                break;
            }
            para_lines.push(next_line);
            i += 1;
        }

        blocks.push(TextBlock::Paragraph(para_lines.join(" ")));
    }

    blocks
}

pub fn render_message_content(ui: &mut egui::Ui, text: &str) -> Option<(String, String)> {
    let segments = parse_markdown_segments(text);
    let mut executed_tool = None;
    let base_wrap_width = ui.available_width().max(150.0);

    for (idx, segment) in segments.iter().enumerate() {
        ui.push_id(("msg_segment", idx), |ui| {
            match segment {
                ContentSegment::Text(t) => {
                    let blocks = parse_text_blocks(t);
                    for (b_idx, block) in blocks.into_iter().enumerate() {
                        ui.push_id(("block", b_idx), |ui| {
                            match block {
                                TextBlock::Heading { level, text } => {
                                    let (size, color, pad_top, pad_bot) = match level {
                                        1 => (19.0, Color32::from_rgb(255, 255, 255), 10.0, 6.0),
                                        2 => (16.5, Color32::from_rgb(147, 197, 253), 8.0, 4.0),
                                        3 => (14.5, Color32::from_rgb(203, 213, 225), 6.0, 3.0),
                                        _ => (13.5, Color32::from_rgb(148, 163, 184), 4.0, 2.0),
                                    };
                                    ui.add_space(pad_top);
                                    let job = create_rich_layout(&text, size, color, true, false, base_wrap_width);
                                    ui.add(egui::Label::new(job).selectable(true).wrap());
                                    ui.add_space(pad_bot);
                                }
                                TextBlock::Table { headers, rows } => {
                                    ui.add_space(8.0);
                                    let col_spacing = 16.0;
                                    let col_widths = compute_column_widths(&headers, &rows, base_wrap_width, col_spacing);

                                    Frame::none()
                                        .fill(Color32::from_rgb(14, 18, 26))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(42, 54, 76)))
                                        .rounding(Rounding::same(8.0))
                                        .inner_margin(Margin::same(10.0))
                                        .show(ui, |ui| {
                                            egui::ScrollArea::horizontal()
                                                .id_salt(format!("table_scroll_{}_{}", idx, b_idx))
                                                .auto_shrink([false, true])
                                                .show(ui, |ui| {
                                                    egui::Grid::new(format!("table_grid_{}_{}", idx, b_idx))
                                                        .striped(true)
                                                        .spacing([col_spacing, 10.0])
                                                        .show(ui, |ui| {
                                                            // Header row
                                                            for (c_idx, h) in headers.iter().enumerate() {
                                                                let col_w = col_widths.get(c_idx).copied().unwrap_or(120.0);
                                                                ui.allocate_ui_with_layout(
                                                                    egui::Vec2::new(col_w, 0.0),
                                                                    egui::Layout::top_down(egui::Align::LEFT),
                                                                    |ui| {
                                                                        ui.set_min_width(col_w);
                                                                        ui.set_max_width(col_w);
                                                                        let job = create_rich_layout(h, 13.5, Color32::from_rgb(147, 197, 253), true, false, col_w);
                                                                        ui.add(egui::Label::new(job).selectable(true).wrap());
                                                                    },
                                                                );
                                                            }
                                                            ui.end_row();

                                                            // Data rows
                                                            for row in &rows {
                                                                for (c_idx, cell) in row.iter().enumerate() {
                                                                    let col_w = col_widths.get(c_idx).copied().unwrap_or(120.0);
                                                                    ui.allocate_ui_with_layout(
                                                                        egui::Vec2::new(col_w, 0.0),
                                                                        egui::Layout::top_down(egui::Align::LEFT),
                                                                        |ui| {
                                                                            ui.set_min_width(col_w);
                                                                            ui.set_max_width(col_w);
                                                                            let job = create_rich_layout(cell, 13.0, Color32::from_rgb(220, 230, 245), false, false, col_w);
                                                                            ui.add(egui::Label::new(job).selectable(true).wrap());
                                                                        },
                                                                    );
                                                                }
                                                                ui.end_row();
                                                            }
                                                        });
                                                });
                                        });
                                    ui.add_space(8.0);
                                }
                                TextBlock::Divider => {
                                    ui.add_space(6.0);
                                    let (rect, _response) = ui.allocate_exact_size(egui::Vec2::new(base_wrap_width, 1.0), egui::Sense::hover());
                                    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(45, 55, 75));
                                    ui.add_space(6.0);
                                }
                                TextBlock::ListItem { bullet, text } => {
                                    let mut job = egui::text::LayoutJob::default();
                                    job.wrap.max_width = base_wrap_width;
                                    job.wrap.break_anywhere = false;

                                    job.append(
                                        &format!("  {}  ", bullet),
                                        0.0,
                                        egui::TextFormat {
                                            font_id: egui::FontId::new(13.5, egui::FontFamily::Name("bold".into())),
                                            color: Color32::from_rgb(96, 165, 250),
                                            ..Default::default()
                                        },
                                    );

                                    append_rich_spans(&mut job, &text, 13.5, Color32::from_rgb(225, 232, 245), false, false);
                                    ui.add(egui::Label::new(job).selectable(true).wrap());
                                    ui.add_space(3.0);
                                }
                                TextBlock::Paragraph(text) => {
                                    let job = create_rich_layout(&text, 13.5, Color32::from_rgb(225, 232, 245), false, false, base_wrap_width);
                                    ui.add(egui::Label::new(job).selectable(true).wrap());
                                    ui.add_space(4.0);
                                }
                            }
                        });
                    }
                }
                ContentSegment::CodeBlock { language, code } => {
                    ui.add_space(2.0);

                    // Outer dark container
                    Frame::none()
                        .fill(Color32::from_rgb(14, 16, 22))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(42, 48, 64)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::same(0.0))
                        .show(ui, |ui| {
                            // Top bar: language on left, action buttons on right
                            Frame::none()
                                .fill(Color32::from_rgb(20, 23, 31))
                                .rounding(Rounding {
                                    nw: 6.0,
                                    ne: 6.0,
                                    sw: 0.0,
                                    se: 0.0,
                                })
                                .inner_margin(Margin::symmetric(10.0, 5.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(language)
                                                .monospace()
                                                .color(Color32::from_rgb(130, 145, 175))
                                                .size(12.0),
                                        );

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let btn = ui.add(
                                                    egui::Button::new(
                                                        RichText::new("📋 Copy")
                                                            .monospace()
                                                            .color(Color32::from_rgb(180, 200, 230))
                                                            .size(11.0),
                                                    )
                                                    .fill(Color32::from_rgb(32, 38, 52))
                                                    .stroke(Stroke::new(
                                                        1.0,
                                                        Color32::from_rgb(50, 60, 80),
                                                    ))
                                                    .rounding(Rounding::same(4.0)),
                                                );

                                                if btn.clicked() {
                                                    ui.output_mut(|o| {
                                                        o.copied_text = code.clone();
                                                    });
                                                }

                                                // If language is executable, add "▶ Run" button!
                                                let lower_lang = language.to_lowercase();
                                                let is_python = lower_lang == "python" || lower_lang == "py";
                                                let is_shell = lower_lang == "sh" || lower_lang == "bash" || lower_lang == "powershell" || lower_lang == "cmd" || lower_lang == "cargo";

                                                if is_python || is_shell {
                                                    ui.add_space(4.0);
                                                    let run_btn = ui.add(
                                                        egui::Button::new(
                                                            RichText::new("▶ Run")
                                                                .monospace()
                                                                .color(Color32::from_rgb(52, 211, 153))
                                                                .strong()
                                                                .size(11.0),
                                                        )
                                                        .fill(Color32::from_rgb(20, 36, 28))
                                                        .stroke(Stroke::new(1.0, Color32::from_rgb(34, 197, 94)))
                                                        .rounding(Rounding::same(4.0)),
                                                    );

                                                    if run_btn.clicked() {
                                                        let tool_name = if is_python {
                                                             "execute_python".to_string()
                                                        } else {
                                                             "run_command".to_string()
                                                        };
                                                        executed_tool = Some((tool_name, code.clone()));
                                                    }
                                                }
                                            },
                                        );
                                    });
                                });

                            // Code body with horizontal scrolling for long lines
                            Frame::none()
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    egui::ScrollArea::horizontal()
                                        .id_salt(format!("code_scroll_{}_{}", idx, language))
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(code)
                                                        .monospace()
                                                        .color(Color32::from_rgb(220, 230, 245))
                                                        .size(13.0),
                                                )
                                                .selectable(true),
                                            );
                                        });
                                });
                        });

                    ui.add_space(6.0);
                }
            }
        });
    }

    executed_tool
}

pub fn render_welcome_hero(ui: &mut egui::Ui, active_model: &str, server_url: &str) -> Option<String> {
    let mut selected_action = None;

    ui.vertical_centered(|ui| {
        ui.add_space(16.0);

        // Logo & Title
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() / 2.0 - 150.0).max(0.0));
            ui.label(
                RichText::new("◈")
                    .size(34.0)
                    .color(Color32::from_rgb(96, 165, 250)),
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("SILO")
                        .size(23.0)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.label(
                    RichText::new("Local Agentic Systems Engineering Workbench")
                        .size(11.0)
                        .color(Color32::from_rgb(148, 163, 184)),
                );
            });
        });

        ui.add_space(10.0);

        // Status badge
        Frame::none()
            .fill(Color32::from_rgb(16, 21, 30))
            .stroke(Stroke::new(1.0, Color32::from_rgb(45, 60, 85)))
            .rounding(Rounding::same(20.0))
            .shadow(egui::epaint::Shadow {
                offset: egui::vec2(0.0, 1.5),
                blur: 8.0,
                spread: 0.0,
                color: Color32::from_black_alpha(35),
            })
            .inner_margin(Margin::symmetric(14.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("●")
                            .color(Color32::from_rgb(34, 197, 94))
                            .size(10.0),
                    );
                    let host_label = if server_url.contains(":1234") {
                        "LM Studio Host Active"
                    } else if server_url.contains(":8080") {
                        "llama.cpp Server Active"
                    } else if server_url.contains(":11434") {
                        "Ollama Host Active"
                    } else if server_url.contains(":8000") {
                        "vLLM Host Active"
                    } else if server_url.contains("openrouter.ai") {
                        "OpenRouter Cloud Active"
                    } else {
                        "Inference Host Active"
                    };
                    ui.label(
                        RichText::new(format!("{}  |  {}", host_label, active_model))
                            .monospace()
                            .color(Color32::from_rgb(200, 220, 255))
                            .size(11.5),
                    );
                });
            });

        ui.add_space(14.0);

        // Action Cards Grid
        ui.label(
            RichText::new("QUICK ACTION WORKFLOWS")
                .monospace()
                .size(11.0)
                .color(Color32::from_rgb(100, 116, 139)),
        );
        ui.add_space(8.0);

        let cards = [
            (
                "🔬 Deep Technical Research",
                "/deep Analyze state-of-the-art zero-copy IPC primitives in modern kernels vs DPDK",
                "Autonomous multi-step investigation using DuckDuckGo search + web_scout markdown extraction.",
                Color32::from_rgb(99, 102, 241),
            ),
            (
                "⚡ Python Verification Sandbox",
                "/calc verify entropy bounds and NIST SP 800-22 test distribution for 100k samples",
                "Execute local Python computation scripts to verify algorithms, math, and benchmarks.",
                Color32::from_rgb(16, 185, 129),
            ),
            (
                "💻 Host Shell Diagnostic",
                "/run cargo test --release -- --nocapture",
                "Execute isolated shell commands, compile binaries, and inspect hardware diagnostics.",
                Color32::from_rgb(245, 158, 11),
            ),
            (
                "🛡 Security Rules Audit",
                "Audit eBPF execve probe and streaming BLAKE3 hashing pipeline against suspicious binaries",
                "Architectural analysis with zero corporate fluff and strict systems boundaries.",
                Color32::from_rgb(239, 68, 68),
            ),
        ];

        ui.columns(2, |columns| {
            for (idx, card) in cards.iter().enumerate() {
                let col = &mut columns[idx % 2];
                col.push_id(idx, |ui| {
                    Frame::none()
                        .fill(Color32::from_rgb(17, 21, 30))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(32, 40, 58)))
                        .rounding(Rounding::same(8.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 2.0),
                            blur: 10.0,
                            spread: 0.0,
                            color: Color32::from_black_alpha(45),
                        })
                        .inner_margin(Margin::same(12.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.set_min_height(75.0);

                            ui.label(RichText::new(card.0).color(card.3).strong().size(13.0));
                            ui.add_space(3.0);
                            ui.label(
                                RichText::new(card.2)
                                    .color(Color32::from_rgb(148, 163, 184))
                                    .size(11.0),
                            );
                            ui.add_space(6.0);

                            let btn = ui.add(
                                egui::Button::new(
                                    RichText::new("▶ Launch Workflow")
                                        .monospace()
                                        .size(11.0)
                                        .color(Color32::from_rgb(220, 230, 255)),
                                )
                                .fill(Color32::from_rgb(26, 32, 48))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(45, 58, 85)))
                                .rounding(Rounding::same(5.0)),
                            );

                            if btn.clicked() {
                                selected_action = Some(card.1.to_string());
                            }
                        });
                    ui.add_space(10.0);
                });
            }
        });
    });

    selected_action
}
