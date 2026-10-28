use crate::engine::{
    execute_tool, fetch_available_models, stream_completion, strip_tool_tags, ModelInfo,
    StreamUpdate,
};
use crate::runtime_env::{build_configured_system_prompt, mask_output_data};
use crate::session::{
    ChatImageAttachment, ChatMessage, ContextConfig, CustomPromptMode, MessageRole, Session,
    SessionManager, ToolCallRecord, ToolStatus,
};
use crate::ui_render::{render_message_content, render_welcome_hero};
use eframe::egui::{self, Color32, Frame, Margin, Pos2, Rect, RichText, Rounding, Stroke, Vec2};
use std::path::PathBuf;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

const FONT_INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const FONT_INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const FONT_JETBRAINS_MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

const DEEP_RESEARCH_DIRECTIVE: &str = r#"DEEP RESEARCH DIRECTIVE:
You are an investigative technical analyst. Follow a rigorous multi-phase research approach:
1. IMMEDIATE TOOL DELEGATION: Never perform mental arithmetic, formula derivations, or parameter checks in <think>. Immediately offload all math, size calculations, and data processing to `execute_python`.
2. Scout: Run `web_search` queries covering technical specifications, standards, and data.
3. Read: Call `fetch_page(url)` on relevant URLs from search results to inspect source documents.
4. Compute: Validate mathematical models or verify equations using `execute_python`.
5. STRICT TOOL LOOP ENFORCEMENT: Never output conversational filler or unexecuted promises in chat (such as "Let me fetch...", "I will search...", "Now checking..."). Always invoke the tool call directly. Only output natural language markdown when synthesizing the final comprehensive dossier.
6. Synthesize: Formulate an exhaustive, highly structured technical dossier with citations and data tables."#;

const MAX_DEEP_FETCHES: usize = 8;

const CALC_DIRECTIVE: &str = r#"[CALC DIRECTIVE]:
The user requested a Python calculation or script execution. You MUST immediately invoke `execute_python(code)` to compute the exact result.
Do NOT write raw Python code blocks in chat without calling the `execute_python` tool.
Once the execution result is returned, present the verified numerical answer."#;

const RUN_DIRECTIVE: &str = r#"[RUN DIRECTIVE]:
The user requested a host command execution. You MUST immediately invoke `run_command(cmd)` to execute this command on the host.
Do NOT write shell commands in chat without calling the `run_command` tool.
Once the execution output is returned, inspect stdout/stderr and present the results."#;

#[derive(Debug, Clone, PartialEq)]
pub enum RollbackTarget {
    EditUser(String),
    RetryUser(String),
    RewindAssistant(String),
    RegenerateAssistant(String),
}

pub struct SiloApp {
    pub session_manager: SessionManager,
    pub sessions: Vec<Session>,
    pub active_session_id: Option<String>,
    pub available_models: Vec<ModelInfo>,
    pub models_rx: Option<UnboundedReceiver<Vec<ModelInfo>>>,
    pub selected_model: String,
    pub input_text: String,
    pub session_filter: String,
    pub show_sidebar: bool,
    pub show_process_panel: bool,
    pub show_context_settings: bool,
    pub is_generating: bool,
    pub is_deep_active: bool,
    pub deep_fetch_count: usize,
    pub transitional_nudge_count: usize,
    pub empty_recovery_count: usize,
    pub forced_tool: Option<String>,
    pub confirm_rollback: Option<String>,
    pub cancel_token: Option<CancellationToken>,
    pub rx: Option<UnboundedReceiver<StreamUpdate>>,
    pub tool_rx: Option<UnboundedReceiver<(String, String, bool, String)>>,
    pub live_thinking: String,
    pub live_content: String,
    pub live_tool_draft: String,
    pub pending_tool_calls: Vec<ToolCallRecord>,
    pub process_logs: Vec<String>,
    pub last_request_payload: Option<String>,
    pub staged_images: Vec<ChatImageAttachment>,
    pub status_message: String,
    pub tokio_handle: Handle,
    pub vault_manager: crate::vault::VaultManager,
    pub vault_unlocked: bool,
    pub master_password_input: String,
    pub password_error: Option<String>,
    pub show_vault_modal: bool,
    pub new_password_input: String,
    pub confirm_password_input: String,
    pub vault_status_msg: Option<String>,
    pub server_url: String,
    pub api_key: String,
    pub show_server_modal: bool,
    pub server_status_msg: Option<String>,
    pub models_tx: UnboundedSender<Vec<ModelInfo>>,
    pub editing_model_inline: bool,
}

impl SiloApp {
    pub fn new(cc: &eframe::CreationContext<'_>, handle: Handle) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Linear / Raycast Aesthetic Dark Theme
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(11, 13, 19);
        visuals.window_fill = Color32::from_rgb(15, 18, 26);
        visuals.extreme_bg_color = Color32::from_rgb(8, 10, 15);

        // Widget colors & refined borders
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(17, 21, 30);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(28, 35, 48));
        visuals.widgets.noninteractive.rounding = Rounding::same(7.0);

        visuals.widgets.inactive.bg_fill = Color32::from_rgb(19, 23, 33);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(32, 40, 56));
        visuals.widgets.inactive.rounding = Rounding::same(7.0);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(27, 34, 49);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(52, 65, 92));
        visuals.widgets.hovered.rounding = Rounding::same(7.0);

        visuals.widgets.active.bg_fill = Color32::from_rgb(36, 46, 68);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(75, 96, 140));
        visuals.widgets.active.rounding = Rounding::same(7.0);

        visuals.selection.bg_fill = Color32::from_rgb(35, 60, 110);
        visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(96, 165, 250));

        cc.egui_ctx.set_visuals(visuals);

        // Embedded Inter & JetBrains Mono Fonts (Zero external dependencies, zero tofu)
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "inter_regular".to_owned(),
            egui::FontData::from_static(FONT_INTER_REGULAR),
        );
        fonts.font_data.insert(
            "inter_semibold".to_owned(),
            egui::FontData::from_static(FONT_INTER_SEMIBOLD),
        );
        fonts.font_data.insert(
            "jetbrains_mono".to_owned(),
            egui::FontData::from_static(FONT_JETBRAINS_MONO),
        );

        // Proportional (UI text)
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .splice(
                0..0,
                vec!["inter_regular".to_owned(), "inter_semibold".to_owned()],
            );

        // Monospace (Code, Terminal, Statuses)
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .splice(0..0, vec!["jetbrains_mono".to_owned()]);

        // Bold
        fonts.families.insert(
            egui::FontFamily::Name("bold".into()),
            vec!["inter_semibold".to_owned(), "inter_regular".to_owned()],
        );

        // System symbol fallbacks
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
            fonts.font_data.insert(
                "segoe_sym".to_owned(),
                egui::FontData::from_owned(data),
            );
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push("segoe_sym".to_owned());
            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("segoe_sym".to_owned());
        }
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\arial.ttf") {
            fonts
                .font_data
                .insert("arial".to_owned(), egui::FontData::from_owned(data));
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push("arial".to_owned());
        }
        cc.egui_ctx.set_fonts(fonts);

        // Linear spacing & typography scale
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style.spacing.scroll.bar_width = 5.0;

        let mut text_styles = std::collections::BTreeMap::new();
        text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(16.0, egui::FontFamily::Name("bold".into())),
        );
        text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
        );
        text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
        );
        text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(12.5, egui::FontFamily::Proportional),
        );
        text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
        );
        style.text_styles = text_styles;
        cc.egui_ctx.set_style(style);

        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut manager = SessionManager::new(current_dir);
        let vault_manager = crate::vault::VaultManager::new();

        let is_pass_protected = vault_manager.is_password_protected();
        let (vault_unlocked, mut sessions) = if is_pass_protected {
            (false, Vec::new())
        } else {
            if let Ok(key) = vault_manager.get_or_create_auto_key() {
                manager.set_vault_key(key);
            }
            (true, manager.load_all_sessions())
        };

        let default_model = "huihui-qwen3.8-27b-abliterated".to_string();

        let active_id = if let Some(first) = sessions.first() {
            Some(first.id.clone())
        } else if vault_unlocked {
            let new_sess = Session::new(&default_model);
            let _ = manager.save_session(&new_sess);
            let id = new_sess.id.clone();
            sessions.push(new_sess);
            Some(id)
        } else {
            None
        };

        let server_url = "http://127.0.0.1:1234".to_string();
        let (models_tx, models_rx) = unbounded_channel();
        let models_tx_init = models_tx.clone();
        let handle_clone = handle.clone();
        let url_clone = server_url.clone();
        handle_clone.spawn(async move {
            let m = fetch_available_models(&url_clone, None).await;
            let _ = models_tx_init.send(m);
        });

        Self {
            session_manager: manager,
            sessions,
            active_session_id: active_id,
            available_models: Vec::new(),
            models_rx: Some(models_rx),
            models_tx,
            selected_model: default_model,
            server_url,
            api_key: String::new(),
            show_server_modal: false,
            server_status_msg: None,
            editing_model_inline: false,
            input_text: String::new(),
            session_filter: String::new(),
            show_sidebar: true,
            show_process_panel: false,
            show_context_settings: false,
            is_generating: false,
            is_deep_active: false,
            deep_fetch_count: 0,
            transitional_nudge_count: 0,
            empty_recovery_count: 0,
            forced_tool: None,
            confirm_rollback: None,
            cancel_token: None,
            rx: None,
            tool_rx: None,
            live_thinking: String::new(),
            live_content: String::new(),
            live_tool_draft: String::new(),
            pending_tool_calls: Vec::new(),
            process_logs: Vec::new(),
            last_request_payload: None,
            staged_images: Vec::new(),
            status_message: "Ready".to_string(),
            tokio_handle: handle,
            vault_manager,
            vault_unlocked,
            master_password_input: String::new(),
            password_error: None,
            show_vault_modal: false,
            new_password_input: String::new(),
            confirm_password_input: String::new(),
            vault_status_msg: None,
        }
    }

    pub fn active_session(&self) -> Option<&Session> {
        if let Some(id) = &self.active_session_id {
            self.sessions.iter().find(|s| &s.id == id)
        } else {
            None
        }
    }

    pub fn active_session_mut(&mut self) -> Option<&mut Session> {
        if let Some(id) = &self.active_session_id {
            self.sessions.iter_mut().find(|s| &s.id == id)
        } else {
            None
        }
    }

    pub fn save_active_session(&self) {
        if let Some(sess) = self.active_session() {
            let _ = self.session_manager.save_session(sess);
        }
    }

    pub fn new_chat(&mut self) {
        let new_sess = Session::new(&self.selected_model);
        let id = new_sess.id.clone();
        let _ = self.session_manager.save_session(&new_sess);
        self.sessions.insert(0, new_sess);
        self.active_session_id = Some(id);
        self.is_deep_active = false;
        self.live_content.clear();
        self.live_thinking.clear();
        self.status_message = "New chat created".to_string();
    }

    pub fn switch_session(&mut self, id: String) {
        if self.is_generating {
            self.cancel_current_generation();
        }
        self.active_session_id = Some(id);
        self.is_deep_active = self.active_session().is_some_and(|s| s.is_deep_research);
        self.live_content.clear();
        self.live_thinking.clear();
    }

    pub fn cancel_current_generation(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        self.is_generating = false;
        self.rx = None;
        self.status_message = "Cancelled".to_string();
    }

    pub fn execute_rollback(&mut self, target: RollbackTarget) {
        self.cancel_current_generation();
        self.confirm_rollback = None;

        let mut edit_content = None;
        let mut edit_images = None;
        let mut status_update = None;
        let mut trigger_generation = false;

        if let Some(sess) = self.active_session_mut() {
            match target {
                RollbackTarget::EditUser(msg_id) => {
                    if let Some(idx) = sess.messages.iter().position(|m| m.id == msg_id) {
                        edit_content = Some(sess.messages[idx].content.clone());
                        edit_images = Some(sess.messages[idx].images.clone());
                        sess.messages.truncate(idx);
                        sess.updated_at = chrono::Utc::now();
                        status_update = Some(format!("Rolled back to message #{}", idx + 1));
                    }
                }
                RollbackTarget::RetryUser(msg_id) => {
                    if let Some(idx) = sess.messages.iter().position(|m| m.id == msg_id) {
                        sess.messages.truncate(idx + 1);
                        sess.updated_at = chrono::Utc::now();
                        trigger_generation = true;
                    }
                }
                RollbackTarget::RewindAssistant(msg_id) => {
                    if let Some(idx) = sess.messages.iter().position(|m| m.id == msg_id) {
                        sess.messages.truncate(idx + 1);
                        sess.updated_at = chrono::Utc::now();
                        status_update =
                            Some(format!("Rewound conversation to checkpoint #{}", idx + 1));
                    }
                }
                RollbackTarget::RegenerateAssistant(msg_id) => {
                    if let Some(idx) = sess.messages.iter().position(|m| m.id == msg_id) {
                        sess.messages.truncate(idx);
                        sess.updated_at = chrono::Utc::now();
                        trigger_generation = true;
                    }
                }
            }
        }

        if let Some(text) = edit_content {
            self.input_text = text;
        }
        if let Some(imgs) = edit_images {
            self.staged_images = imgs;
        }
        if let Some(status) = status_update {
            self.status_message = status;
        }

        self.save_active_session();

        if trigger_generation {
            self.start_generation();
        }
    }

    pub fn paste_image_from_clipboard(&mut self) {
        match ChatImageAttachment::from_clipboard() {
            Ok(att) => {
                let dims = format!("{}x{}", att.width, att.height);
                let kb = att.byte_size / 1024;
                self.status_message = format!("Attached clipboard image ({} px, {} KB)", dims, kb);
                self.staged_images.push(att);
            }
            Err(err) => {
                self.status_message = format!("Clipboard paste: {}", err);
            }
        }
    }

    pub fn attach_image_from_path(&mut self, path: &std::path::Path) {
        match ChatImageAttachment::from_file_path(path) {
            Ok(att) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("image");
                let dims = format!("{}x{}", att.width, att.height);
                let kb = att.byte_size / 1024;
                self.status_message = format!("Attached {} ({} px, {} KB)", name, dims, kb);
                self.staged_images.push(att);
            }
            Err(err) => {
                self.status_message = format!("Failed to attach image: {}", err);
            }
        }
    }

    pub fn send_message(&mut self) {
        let raw_text = self.input_text.trim().to_string();
        let images = std::mem::take(&mut self.staged_images);
        if (raw_text.is_empty() && images.is_empty()) || self.is_generating {
            return;
        }

        self.input_text.clear();

        let lower = raw_text.to_lowercase();
        let is_deep = lower.starts_with("/deep");
        let is_calc = lower.starts_with("/calc");
        let is_run = lower.starts_with("/run");

        let session_is_deep = self.active_session().is_some_and(|s| s.is_deep_research);
        let effective_deep = is_deep || session_is_deep;

        let (user_text, directive_opt, forced_tool) = if is_deep {
            let topic = raw_text[5..].trim().to_string();
            let effective = if topic.is_empty() {
                "Conduct a deep technical investigation".to_string()
            } else {
                topic
            };
            (effective, Some(DEEP_RESEARCH_DIRECTIVE), None)
        } else if session_is_deep {
            // Continuation turn in a persistent Deep Research session:
            // don't reinject the massive directive, keep user input natural while preserving tool mode
            (raw_text, None, None)
        } else if is_calc {
            let code_or_topic = raw_text[5..].trim().to_string();
            let effective = if code_or_topic.is_empty() {
                "Calculate and run Python computation".to_string()
            } else {
                code_or_topic
            };
            (
                effective,
                Some(CALC_DIRECTIVE),
                Some("execute_python".to_string()),
            )
        } else if is_run {
            let cmd_or_topic = raw_text[4..].trim().to_string();
            let effective = if cmd_or_topic.is_empty() {
                "Execute host shell command".to_string()
            } else {
                cmd_or_topic
            };
            (
                effective,
                Some(RUN_DIRECTIVE),
                Some("run_command".to_string()),
            )
        } else if raw_text.is_empty() && !images.is_empty() {
            ("Please analyze the attached image.".to_string(), None, None)
        } else {
            (raw_text, None, None)
        };

        self.is_deep_active = effective_deep;
        self.deep_fetch_count = 0;
        self.transitional_nudge_count = 0;
        self.empty_recovery_count = 0;
        self.forced_tool = forced_tool;

        // Append user message (and directive if applicable)
        if let Some(sess) = self.active_session_mut() {
            if effective_deep {
                sess.is_deep_research = true;
            }
            let final_user_text = if let Some(directive) = directive_opt {
                format!("{}\n\n---\n\n{}", directive, user_text)
            } else {
                user_text
            };
            sess.messages
                .push(ChatMessage::user_with_images(&final_user_text, images));
            sess.update_title_from_first_message();
            sess.updated_at = chrono::Utc::now();
        }
        self.save_active_session();

        self.start_generation();
    }

    pub fn start_generation(&mut self) {
        let sess = match self.active_session() {
            Some(s) => s.clone(),
            None => return,
        };

        self.is_generating = true;
        self.live_content.clear();
        self.live_thinking.clear();
        self.live_tool_draft.clear();
        self.pending_tool_calls.clear();

        let cancel_token = CancellationToken::new();
        self.cancel_token = Some(cancel_token.clone());

        let (tx, rx) = unbounded_channel();
        self.rx = Some(rx);

        let model = self.selected_model.clone();
        let system_prompt = build_configured_system_prompt(&sess.context_config);
        let messages = sess.messages.clone();

        let enable_tools = if !sess.context_config.enable_tools {
            false
        } else { !(self.is_deep_active && self.deep_fetch_count >= MAX_DEEP_FETCHES) };

        let forced_tool = self.forced_tool.take();
        let base_url = self.server_url.clone();
        let api_key = if self.api_key.trim().is_empty() {
            None
        } else {
            Some(self.api_key.trim().to_string())
        };

        self.tokio_handle.spawn(async move {
            stream_completion(
                &base_url,
                api_key.as_deref(),
                &model,
                &system_prompt,
                &messages,
                enable_tools,
                forced_tool,
                tx,
                cancel_token,
            )
            .await;
        });

        self.status_message = "Generating...".to_string();
    }

    pub fn dispatch_manual_tool_execution(&mut self, tool_name: String, code_or_cmd: String) {
        if self.is_generating {
            return;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let args_json = if tool_name == "execute_python" {
            serde_json::json!({ "code": code_or_cmd }).to_string()
        } else {
            serde_json::json!({ "cmd": code_or_cmd }).to_string()
        };

        let tc = ToolCallRecord {
            id: id.clone(),
            name: tool_name.clone(),
            arguments: args_json.clone(),
            output: None,
            status: ToolStatus::Running,
        };

        self.pending_tool_calls = vec![tc.clone()];
        if let Some(sess) = self.active_session_mut() {
            let mut assistant_msg =
                ChatMessage::assistant(format!("Running `{}`...", tool_name), None);
            assistant_msg.tool_calls.push(tc);
            sess.messages.push(assistant_msg);
        }
        self.save_active_session();

        self.show_process_panel = true;
        self.status_message = format!("Running tool: {}...", tool_name);

        let handle = self.tokio_handle.clone();
        let (tx, rx) = unbounded_channel();
        self.tool_rx = Some(rx);

        handle.spawn(async move {
            let (ok, output) = execute_tool(&tool_name, &args_json).await;
            let _ = tx.send((id, tool_name, ok, output));
        });
    }

    pub fn handle_stream_events(&mut self) {
        if let Some(rx) = &mut self.rx {
            let mut finished = false;
            let mut final_content = String::new();
            let mut final_thinking = None;
            let mut has_error = None;
            let mut new_tool_calls = Vec::new();

            while let Ok(event) = rx.try_recv() {
                match event {
                    StreamUpdate::Token(t) => {
                        if self.live_content.is_empty() && t.trim().is_empty() {
                            // Skip leading empty whitespace
                        } else {
                            self.live_content.push_str(&t);
                        }
                    }
                    StreamUpdate::Thinking(th) => {
                        self.live_thinking.push_str(&th);
                    }
                    StreamUpdate::ToolDrafting(draft) => {
                        self.live_tool_draft.push_str(&draft);
                    }
                    StreamUpdate::Log(l) => {
                        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
                        self.process_logs.push(format!("[{}] {}", ts, l));
                    }
                    StreamUpdate::Payload(p) => {
                        self.last_request_payload = Some(p);
                    }
                    StreamUpdate::Finished {
                        full_content,
                        full_thinking,
                        tool_calls,
                    } => {
                        finished = true;
                        final_content = full_content;
                        final_thinking = full_thinking;
                        for tc in tool_calls {
                            new_tool_calls.push(ToolCallRecord {
                                id: tc.id,
                                name: tc.name,
                                arguments: tc.arguments,
                                output: None,
                                status: ToolStatus::Pending,
                            });
                        }
                    }
                    StreamUpdate::Error(err) => {
                        has_error = Some(err);
                        finished = true;
                    }
                }
            }

            for tc in new_tool_calls {
                self.pending_tool_calls.push(tc);
            }

            if finished {
                self.is_generating = false;
                self.rx = None;
                self.live_tool_draft.clear();

                if let Some(err) = has_error {
                    self.status_message = format!("Error: {}", err);
                    if let Some(sess) = self.active_session_mut() {
                        sess.messages
                            .push(ChatMessage::assistant(format!("⚠️ {}", err), None));
                    }
                    self.save_active_session();
                } else {
                    let content = if !final_content.is_empty() {
                        final_content
                    } else {
                        self.live_content.clone()
                    };

                    let thinking = if final_thinking.is_some() {
                        final_thinking
                    } else if !self.live_thinking.is_empty() {
                        Some(self.live_thinking.clone())
                    } else {
                        None
                    };

                    // If the model already produced a substantive final answer / dossier, do not loop on trailing citations
                    let trimmed_c = content.trim();
                    let is_full_report = trimmed_c.len() > 600
                        && (trimmed_c.contains('#')
                            || trimmed_c.contains("|---|")
                            || trimmed_c.contains("1.")
                            || trimmed_c.contains("##"));
                    if is_full_report {
                        self.pending_tool_calls.clear();
                    }

                    // Check if tools need to be executed
                    if !self.pending_tool_calls.is_empty() {
                        let mut msg = ChatMessage::assistant(&content, thinking);
                        msg.tool_calls = self.pending_tool_calls.clone();

                        if let Some(sess) = self.active_session_mut() {
                            sess.messages.push(msg);
                        }
                        self.save_active_session();

                        self.execute_next_tool();
                    } else {
                        let trimmed = content.trim();
                        let lower_c = trimmed.to_lowercase();
                        let is_transitional = self.is_deep_active
                            && self.transitional_nudge_count < 5
                            && !trimmed.is_empty()
                            && (trimmed.len() < 700
                                && (!trimmed.contains('#')
                                    || lower_c.contains("запрос")
                                    || lower_c.contains("поищ")
                                    || lower_c.contains("ищу")
                                    || lower_c.contains("дополн")
                                    || lower_c.contains("провер")
                                    || lower_c.contains("изуч")
                                    || lower_c.contains("переход")
                                    || lower_c.contains("соберу")
                                    || lower_c.contains("собираю")
                                    || lower_c.contains("сейчас")
                                    || lower_c.contains("найду")
                                    || lower_c.contains("запускаю")
                                    || lower_c.contains("search")
                                    || lower_c.contains("fetch")
                                    || lower_c.contains("query")
                                    || lower_c.contains("look into")
                                    || lower_c.contains("let me")
                                    || lower_c.contains("investigate")
                                    || lower_c.contains("параллельно")
                                    || lower_c.contains("отправляю")
                                    || lower_c.contains("читаю")));

                        if is_transitional {
                            self.transitional_nudge_count += 1;
                            let fetch_count = self.deep_fetch_count;
                            if let Some(sess) = self.active_session_mut() {
                                sess.messages
                                    .push(ChatMessage::assistant(&content, thinking));
                                if fetch_count < MAX_DEEP_FETCHES {
                                    sess.messages.push(ChatMessage::system("[CRITICAL RESEARCH DIRECTIVE]: Do NOT output conversational filler or statements of intent (such as 'Let me fetch...', 'I will look into...'). You MUST IMMEDIATELY invoke `web_search(query)` or `fetch_page(url)` as a structured tool call. If all research is truly finished, output the final dossier immediately."));
                                } else {
                                    sess.messages.push(ChatMessage::system("[FINAL DOSSIER REQUIRED]: The research gathering phase is complete. Do NOT announce future searches. Synthesize and write the exhaustive, multi-section final research dossier now with all findings, metrics, and strategic conclusions."));
                                }
                                sess.updated_at = chrono::Utc::now();
                            }
                            self.save_active_session();
                            self.live_content.clear();
                            self.live_thinking.clear();
                            // Keep tools active during transitional nudge!
                            self.start_generation();
                            return;
                        }

                        // 1. REASONING MATERIALIZATION NUDGE:
                        // If model reasoned extensively (thinking is rich), but generated NO markdown text,
                        // automatically prompt it to synthesize its internal reasoning into the final markdown report!
                        if trimmed.is_empty() {
                            if let Some(th) = &thinking {
                                if th.trim().len() > 100 && self.empty_recovery_count < 2 {
                                    self.empty_recovery_count += 1;
                                    if let Some(sess) = self.active_session_mut() {
                                        sess.messages
                                            .push(ChatMessage::assistant("", Some(th.clone())));
                                        sess.messages.push(ChatMessage::system(
                                            "[SYNTHESIZE DOSSIER]: Your internal reasoning and evidence gathering are complete. Now write the exhaustive, definitive final markdown dossier to the user immediately. Present all findings, model specifications, VRAM math, tokens/second, and concrete links. Do NOT call any more tools and do NOT output conversational filler."
                                        ));
                                        sess.updated_at = chrono::Utc::now();
                                    }
                                    self.save_active_session();
                                    self.live_content.clear();
                                    self.live_thinking.clear();
                                    self.start_generation();
                                    return;
                                }
                            }
                        }

                        // 2. EMPTY RESPONSE RECOVERY:
                        // If model returned 0-2 tokens (empty response) right after receiving tool output,
                        // do not emit a dead failure note — inject recovery directive and continue synthesis.
                        let was_after_tool = self.active_session().is_some_and(|sess| {
                            sess.messages
                                .last()
                                .is_some_and(|m| m.role == MessageRole::Tool)
                        });

                        if trimmed.is_empty() && was_after_tool && self.empty_recovery_count < 2 {
                            self.empty_recovery_count += 1;
                            if let Some(sess) = self.active_session_mut() {
                                sess.messages.push(ChatMessage::system(
                                    "[DIRECTIVE]: Tool execution completed. Review the gathered data above, synthesize the facts, and formulate your comprehensive answer to the user now."
                                ));
                                sess.updated_at = chrono::Utc::now();
                            }
                            self.save_active_session();
                            self.live_content.clear();
                            self.live_thinking.clear();
                            self.start_generation();
                            return;
                        }

                        let final_content = if trimmed.is_empty() {
                            if let Some(th) = &thinking {
                                let th_trim = th.trim();
                                if !th_trim.is_empty() {
                                    format!(
                                        "{}\n\n*(Rendered from model reasoning trace)*",
                                        th_trim
                                    )
                                } else {
                                    "*(Model returned an empty response. Try rewording or regenerating.)*".to_string()
                                }
                            } else {
                                "*(Model returned an empty response. Try rewording or regenerating.)*".to_string()
                            }
                        } else {
                            content
                        };

                        if let Some(sess) = self.active_session_mut() {
                            sess.messages
                                .push(ChatMessage::assistant(&final_content, thinking));
                            sess.updated_at = chrono::Utc::now();
                        }
                        self.save_active_session();
                        self.status_message = "Ready".to_string();
                    }

                    self.live_content.clear();
                    self.live_thinking.clear();
                }
            }
        }
    }

    pub fn execute_next_tool(&mut self) {
        let tool = match self
            .pending_tool_calls
            .iter_mut()
            .find(|t| t.status == ToolStatus::Pending)
        {
            Some(t) => {
                t.status = ToolStatus::Running;
                t.clone()
            }
            None => {
                self.pending_tool_calls.clear();
                return;
            }
        };

        // Update status in session messages as well
        if let Some(sess) = self.active_session_mut() {
            if let Some(last_msg) = sess.messages.last_mut() {
                if let Some(tc) = last_msg.tool_calls.iter_mut().find(|t| t.id == tool.id) {
                    tc.status = ToolStatus::Running;
                }
            }
        }

        self.show_process_panel = true;
        self.status_message = format!("Running tool: {}...", tool.name);

        let tool_id = tool.id.clone();
        let name = tool.name.clone();
        let args_str = tool.arguments.clone();
        let handle = self.tokio_handle.clone();

        let (tx, rx) = unbounded_channel();
        self.tool_rx = Some(rx);

        handle.spawn(async move {
            let (ok, output) = execute_tool(&name, &args_str).await;
            let _ = tx.send((tool_id, name, ok, output));
        });
    }

    pub fn on_tool_completed(
        &mut self,
        tool_id: String,
        tool_name: String,
        ok: bool,
        output: String,
    ) {
        self.process_logs
            .push(format!("[Tool {}]:\n{}", tool_name, output));

        let effective_output = if let Some(sess) = self.active_session() {
            mask_output_data(&output, &sess.context_config)
        } else {
            output.clone()
        };

        if let Some(tc) = self.pending_tool_calls.iter_mut().find(|t| t.id == tool_id) {
            tc.status = if ok {
                ToolStatus::Success
            } else {
                ToolStatus::Failed
            };
            tc.output = Some(effective_output.clone());
        }

        let instruction = if tool_name == "web_search" || tool_name == "search" {
            if self.is_deep_active && self.deep_fetch_count < MAX_DEEP_FETCHES {
                format!("Results:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Review these results. Select the most authoritative URLs and call `fetch_page(url)` to read their full content. You have {} page reads remaining.", effective_output, MAX_DEEP_FETCHES - self.deep_fetch_count)
            } else {
                format!(
                    "Results:\n{}\n\n[INSTRUCTION]: Synthesize your final answer now.",
                    effective_output
                )
            }
        } else if tool_name == "fetch_page" || tool_name == "fetch" {
            self.deep_fetch_count += 1;
            if self.is_deep_active && self.deep_fetch_count < MAX_DEEP_FETCHES {
                format!("Page Markdown:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Source read [{}/{}]. If additional pages or searches are needed to complete the research, proceed. If sufficient evidence has been gathered, produce the final comprehensive dossier.", effective_output, self.deep_fetch_count, MAX_DEEP_FETCHES)
            } else if self.is_deep_active {
                format!("Page Markdown:\n{}\n\n[DEEP RESEARCH COMPLETED]: Maximum research quota reached ({}/{}). All necessary empirical evidence has been gathered. Do NOT call any more tools and do NOT announce future searches. Synthesize and write the exhaustive, definitive final technical research dossier now, complete with findings, comparisons, exact metrics, and strategic conclusions.", effective_output, self.deep_fetch_count, MAX_DEEP_FETCHES)
            } else {
                format!("Page Markdown:\n{}\n\n[INSTRUCTION]: Now synthesize your final comprehensive dossier.", effective_output)
            }
        } else if tool_name == "execute_python" || tool_name == "python" {
            if self.is_deep_active {
                format!("Python Execution Result:\n{}\n\n[DEEP RESEARCH GUIDANCE]: Computation complete. Incorporate these exact numbers into your analysis. If additional searches or pages are needed, proceed; otherwise, synthesize your comprehensive dossier.", effective_output)
            } else {
                format!("Python Execution Result:\n{}\n\n[INSTRUCTION]: Use this exact numerical result to formulate your final response to the user.", effective_output)
            }
        } else {
            format!("Command Execution Result:\n{}\n\n[INSTRUCTION]: Inspect stdout/stderr. Proceed with the task.", effective_output)
        };

        if let Some(sess) = self.active_session_mut() {
            // Update the tool call in the last assistant message
            for msg in sess.messages.iter_mut().rev() {
                if msg.role == MessageRole::Assistant {
                    if let Some(tc) = msg.tool_calls.iter_mut().find(|t| t.id == tool_id) {
                        tc.status = if ok {
                            ToolStatus::Success
                        } else {
                            ToolStatus::Failed
                        };
                        tc.output = Some(effective_output.clone());
                        break;
                    }
                }
            }

            sess.messages.push(ChatMessage::tool(instruction, &tool_id));
        }
        self.save_active_session();

        // Check if there are more pending tools in this turn
        let has_more = self
            .pending_tool_calls
            .iter()
            .any(|t| t.status == ToolStatus::Pending);
        if has_more {
            self.execute_next_tool();
        } else {
            self.pending_tool_calls.clear();
            self.empty_recovery_count = 0;
            self.start_generation();
        }
    }
}

impl eframe::App for SiloApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_stream_events();

        // 0. LOCK SCREEN IF VAULT IS LOCKED
        if !self.vault_unlocked {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(Color32::from_rgb(10, 14, 20)))
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        let avail_h = ui.available_height();
                        ui.add_space(avail_h * 0.28);

                        ui.label(
                            RichText::new("🔒 SILO SECURE VAULT")
                                .size(24.0)
                                .strong()
                                .color(Color32::from_rgb(248, 250, 252)),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Encrypted local store (XChaCha20-Poly1305 + shake-entropy)",
                            )
                            .size(12.5)
                            .color(Color32::from_rgb(148, 163, 184)),
                        );
                        ui.add_space(24.0);

                        ui.label(
                            RichText::new("Enter Master Password to unlock sessions:")
                                .size(13.0)
                                .color(Color32::from_rgb(226, 232, 240)),
                        );
                        ui.add_space(10.0);

                        let mut submit = false;
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 320.0).max(0.0) / 2.0);
                            let edit = ui.add_sized(
                                [320.0, 36.0],
                                egui::TextEdit::singleline(&mut self.master_password_input)
                                    .password(true)
                                    .hint_text("Master Password..."),
                            );
                            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                submit = true;
                            }
                        });

                        ui.add_space(14.0);

                        if ui
                            .add_sized(
                                [160.0, 34.0],
                                egui::Button::new(RichText::new("🔓  Unlock Vault").strong()),
                            )
                            .clicked()
                        {
                            submit = true;
                        }

                        if submit {
                            match self
                                .vault_manager
                                .unlock_with_password(&self.master_password_input)
                            {
                                Ok(key) => {
                                    self.session_manager.set_vault_key(key);
                                    self.sessions = self.session_manager.load_all_sessions();
                                    if self.active_session_id.is_none() {
                                        if let Some(first) = self.sessions.first() {
                                            self.active_session_id = Some(first.id.clone());
                                        } else {
                                            let new_sess = Session::new(&self.selected_model);
                                            let _ = self.session_manager.save_session(&new_sess);
                                            self.active_session_id = Some(new_sess.id.clone());
                                            self.sessions.push(new_sess);
                                        }
                                    }
                                    self.vault_unlocked = true;
                                    self.master_password_input.clear();
                                    self.password_error = None;
                                }
                                Err(err) => {
                                    self.password_error = Some(err);
                                }
                            }
                        }

                        if let Some(err) = &self.password_error {
                            ui.add_space(12.0);
                            ui.label(
                                RichText::new(format!("⚠ {}", err))
                                    .color(Color32::from_rgb(248, 113, 113))
                                    .size(12.5),
                            );
                        }
                    });
                });
            return;
        }

        // Drag and drop image files onto application window
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped_files.is_empty() {
            for file in dropped_files {
                if let Some(path) = file.path {
                    let is_img = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|ext| {
                            matches!(
                                ext.to_lowercase().as_str(),
                                "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
                            )
                        })
                        .unwrap_or(false);
                    if is_img {
                        self.attach_image_from_path(&path);
                    }
                } else if let Some(bytes) = file.bytes {
                    if let Ok(att) = ChatImageAttachment::from_image_bytes(&bytes) {
                        self.status_message =
                            format!("Attached dropped image ({}x{} px)", att.width, att.height);
                        self.staged_images.push(att);
                    }
                }
            }
        }

        // Global clipboard paste shortcut (Ctrl+V) when clipboard contains an image
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::V)) {
            if let Ok(att) = ChatImageAttachment::from_clipboard() {
                let dims = format!("{}x{}", att.width, att.height);
                let kb = att.byte_size / 1024;
                self.status_message =
                    format!("Pasted image from clipboard ({} px, {} KB)", dims, kb);
                self.staged_images.push(att);
            }
        }

        if let Some(rx) = &mut self.tool_rx {
            ctx.request_repaint();
            if let Ok((tool_id, tool_name, ok, output)) = rx.try_recv() {
                self.tool_rx = None;
                self.on_tool_completed(tool_id, tool_name, ok, output);
            }
        }

        if let Some(rx) = &mut self.models_rx {
            if let Ok(models) = rx.try_recv() {
                if !models.is_empty() {
                    if let Some(loaded) = models.iter().find(|m| m.is_loaded) {
                        self.selected_model = loaded.id.clone();
                    } else if !models.iter().any(|m| m.id == self.selected_model) {
                        if let Some(first) = models.first() {
                            self.selected_model = first.id.clone();
                        }
                    }
                    self.server_status_msg =
                        Some(format!("Connected! Found {} model(s).", models.len()));
                    self.available_models = models;
                } else {
                    self.server_status_msg = Some("Connection check returned 0 models (host unreachable or invalid endpoint).".to_string());
                }
            }
        }

        if self.is_generating {
            ctx.request_repaint();
        }

        // 1. TOP NAVIGATION & STATUS BAR (Linear/Raycast style, no overlapping)
        egui::TopBottomPanel::top("top_bar")
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(11, 14, 20))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(24, 30, 44)))
                    .inner_margin(Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| {
                let mut toggle_deep = false;
                let mut toggle_ctx_settings = false;
                let mut toggle_vault_settings = false;

                ui.horizontal(|ui| {
                    // Right aligned group first: NEVER overlapped!
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Terminal logs toggle
                        let logs_count = self.process_logs.len();
                        let term_text = format!("◈ Logs ({})", logs_count);
                        let term_btn = ui.add(
                            egui::Button::new(
                                RichText::new(term_text)
                                    .color(if self.show_process_panel {
                                        Color32::from_rgb(96, 165, 250)
                                    } else {
                                        Color32::from_rgb(148, 163, 184)
                                    })
                                    .size(11.5),
                            )
                            .fill(if self.show_process_panel {
                                Color32::from_rgb(22, 32, 50)
                            } else {
                                Color32::from_rgb(17, 21, 30)
                            })
                            .stroke(Stroke::new(
                                1.0,
                                if self.show_process_panel {
                                    Color32::from_rgb(59, 130, 246)
                                } else {
                                    Color32::from_rgb(30, 37, 52)
                                },
                            ))
                            .rounding(Rounding::same(6.0)),
                        );
                        if term_btn.clicked() {
                            self.show_process_panel = !self.show_process_panel;
                        }

                        ui.add_space(8.0);

                        // Model Selector (Dropdown + Direct Text Input)
                        if self.editing_model_inline {
                            let re = ui.add(
                                egui::TextEdit::singleline(&mut self.selected_model)
                                    .desired_width(190.0)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("model-name..."),
                            );
                            if re.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                self.editing_model_inline = false;
                            }
                        } else if !self.available_models.is_empty() {
                            egui::ComboBox::from_id_salt("model_selector")
                                .selected_text(
                                    RichText::new(&self.selected_model)
                                        .color(Color32::from_rgb(52, 211, 153))
                                        .monospace()
                                        .size(11.5),
                                )
                                .show_ui(ui, |ui| {
                                    for m in &self.available_models {
                                        let label = if m.is_loaded {
                                            format!("● [Active] {}", m.id)
                                        } else {
                                            format!("○ {}", m.id)
                                        };
                                        ui.selectable_value(
                                            &mut self.selected_model,
                                            m.id.clone(),
                                            label,
                                        );
                                    }
                                });
                        } else {
                            let lbl = ui.add(
                                egui::Button::new(
                                    RichText::new(&self.selected_model)
                                        .color(Color32::from_rgb(52, 211, 153))
                                        .monospace()
                                        .size(11.5),
                                )
                                .fill(Color32::from_rgb(17, 21, 30))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(30, 37, 52)))
                                .rounding(Rounding::same(6.0)),
                            );
                            if lbl.clicked() {
                                self.editing_model_inline = true;
                            }
                        }

                        // Toggle Text Edit / Dropdown mode
                        let edit_icon = if self.editing_model_inline {
                            "✔"
                        } else {
                            "✏"
                        };
                        let edit_btn = ui.add(
                            egui::Button::new(
                                RichText::new(edit_icon)
                                    .color(Color32::from_rgb(148, 163, 184))
                                    .size(11.0),
                            )
                            .fill(Color32::from_rgb(17, 21, 30))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(30, 37, 52)))
                            .rounding(Rounding::same(4.0)),
                        );
                        if edit_btn.clicked() {
                            self.editing_model_inline = !self.editing_model_inline;
                        }

                        ui.add_space(6.0);

                        // Server / Host Settings Button
                        let host_text = if self.server_url.contains("127.0.0.1")
                            || self.server_url.contains("localhost")
                        {
                            if let Some(port_idx) = self.server_url.rfind(':') {
                                format!(
                                    "🌐 :{}",
                                    &self.server_url[port_idx + 1..].trim_end_matches('/')
                                )
                            } else {
                                "🌐 Local".to_string()
                            }
                        } else {
                            "🌐 Cloud".to_string()
                        };

                        let host_btn = ui.add(
                            egui::Button::new(
                                RichText::new(host_text)
                                    .color(if self.show_server_modal {
                                        Color32::from_rgb(56, 189, 248)
                                    } else {
                                        Color32::from_rgb(148, 163, 184)
                                    })
                                    .monospace()
                                    .size(11.5),
                            )
                            .fill(if self.show_server_modal {
                                Color32::from_rgb(16, 28, 44)
                            } else {
                                Color32::from_rgb(17, 21, 30)
                            })
                            .stroke(Stroke::new(
                                1.0,
                                if self.show_server_modal {
                                    Color32::from_rgb(56, 189, 248)
                                } else {
                                    Color32::from_rgb(30, 37, 52)
                                },
                            ))
                            .rounding(Rounding::same(6.0)),
                        );
                        if host_btn.clicked() {
                            self.show_server_modal = !self.show_server_modal;
                        }

                        ui.add_space(8.0);

                        // Live status indicator
                        if self.is_generating {
                            Frame::none()
                                .fill(Color32::from_rgb(36, 28, 16))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(245, 158, 11)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 3.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("⚡ Working")
                                            .color(Color32::from_rgb(251, 191, 36))
                                            .size(11.0),
                                    );
                                });
                        } else {
                            Frame::none()
                                .fill(Color32::from_rgb(14, 26, 20))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(34, 197, 94)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(8.0, 3.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("● Ready")
                                            .color(Color32::from_rgb(52, 211, 153))
                                            .size(11.0),
                                    );
                                });
                        }

                        ui.add_space(10.0);

                        // Left group inside remaining available space
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // Sidebar toggle button
                            let toggle_icon = if self.show_sidebar {
                                "◧ Sidebar"
                            } else {
                                "◨ Sidebar"
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(toggle_icon)
                                            .color(Color32::from_rgb(148, 163, 184))
                                            .size(11.5),
                                    )
                                    .fill(Color32::from_rgb(17, 21, 30))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(30, 37, 52)))
                                    .rounding(Rounding::same(6.0)),
                                )
                                .clicked()
                            {
                                self.show_sidebar = !self.show_sidebar;
                            }

                            ui.add_space(10.0);

                            // Modern Linear/Raycast Branding (Neutral, clean, multi-model runtime)
                            ui.label(
                                RichText::new("◈  SILO")
                                    .color(Color32::from_rgb(248, 250, 252))
                                    .strong()
                                    .size(13.0),
                            );
                            Frame::none()
                                .fill(Color32::from_rgb(19, 23, 34))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(32, 40, 58)))
                                .rounding(Rounding::same(4.0))
                                .inner_margin(Margin::symmetric(6.0, 2.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("LOCAL RUNTIME")
                                            .color(Color32::from_rgb(148, 163, 184))
                                            .size(9.5)
                                            .strong(),
                                    );
                                });

                            ui.add_space(10.0);

                            // Active session title & settings
                            if let Some(sess) = self.active_session() {
                                let rem_w = ui.available_width();
                                let max_chars = ((rem_w - 200.0).max(40.0) / 7.5) as usize;
                                let title_display = if sess.title.chars().count() > max_chars {
                                    format!(
                                        "{}...",
                                        sess.title
                                            .chars()
                                            .take(max_chars.saturating_sub(2))
                                            .collect::<String>()
                                    )
                                } else {
                                    sess.title.clone()
                                };
                                Frame::none()
                                    .fill(Color32::from_rgb(17, 21, 30))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(28, 35, 50)))
                                    .rounding(Rounding::same(6.0))
                                    .inner_margin(Margin::symmetric(8.0, 3.0))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(title_display)
                                                .color(Color32::from_rgb(203, 213, 225))
                                                .size(11.5),
                                        );
                                    });

                                ui.add_space(6.0);

                                let is_deep = sess.is_deep_research;
                                let deep_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new(if is_deep {
                                            "🔬 Deep: ON"
                                        } else {
                                            "🔬 Deep: OFF"
                                        })
                                        .color(if is_deep {
                                            Color32::from_rgb(192, 132, 252)
                                        } else {
                                            Color32::from_rgb(148, 163, 184)
                                        })
                                        .size(11.0),
                                    )
                                    .fill(if is_deep {
                                        Color32::from_rgb(36, 18, 48)
                                    } else {
                                        Color32::from_rgb(17, 21, 30)
                                    })
                                    .stroke(Stroke::new(
                                        1.0,
                                        if is_deep {
                                            Color32::from_rgb(147, 51, 234)
                                        } else {
                                            Color32::from_rgb(30, 37, 52)
                                        },
                                    ))
                                    .rounding(Rounding::same(6.0)),
                                );
                                if deep_btn.clicked() {
                                    toggle_deep = true;
                                }

                                ui.add_space(6.0);

                                let is_spoofed = !sess.context_config.enable_system_prompt
                                    || !sess.context_config.enable_host_env
                                    || !sess.context_config.enable_temporal_anchor
                                    || !sess.context_config.spoof_os.is_empty()
                                    || !sess.context_config.spoof_cwd.is_empty()
                                    || !sess.context_config.spoof_user_home.is_empty()
                                    || !sess.context_config.custom_system_prompt.is_empty()
                                    || !sess.context_config.enable_tools;

                                let ctx_label = if is_spoofed {
                                    "⚙ Context (Spoofed)"
                                } else {
                                    "⚙ Context"
                                };

                                let ctx_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new(ctx_label)
                                            .color(if is_spoofed {
                                                Color32::from_rgb(251, 191, 36)
                                            } else {
                                                Color32::from_rgb(148, 163, 184)
                                            })
                                            .size(11.0),
                                    )
                                    .fill(if is_spoofed {
                                        Color32::from_rgb(36, 28, 16)
                                    } else {
                                        Color32::from_rgb(17, 21, 30)
                                    })
                                    .stroke(Stroke::new(
                                        1.0,
                                        if is_spoofed {
                                            Color32::from_rgb(245, 158, 11)
                                        } else {
                                            Color32::from_rgb(30, 37, 52)
                                        },
                                    ))
                                    .rounding(Rounding::same(6.0)),
                                );
                                if ctx_btn.clicked() {
                                    toggle_ctx_settings = true;
                                }

                                ui.add_space(6.0);
                                let is_vault_locked = self.vault_manager.is_password_protected();
                                let vault_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new(if is_vault_locked {
                                            "🔐 Vault"
                                        } else {
                                            "🔒 Vault"
                                        })
                                        .color(if is_vault_locked {
                                            Color32::from_rgb(74, 222, 128)
                                        } else {
                                            Color32::from_rgb(56, 189, 248)
                                        })
                                        .size(11.0),
                                    )
                                    .fill(Color32::from_rgb(17, 21, 30))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(30, 37, 52)))
                                    .rounding(Rounding::same(6.0)),
                                );
                                if vault_btn.clicked() {
                                    toggle_vault_settings = true;
                                }
                            }
                        });
                    });
                });

                if toggle_deep {
                    let new_val = if let Some(s) = self.active_session_mut() {
                        s.is_deep_research = !s.is_deep_research;
                        s.is_deep_research
                    } else {
                        false
                    };
                    self.is_deep_active = new_val;
                    self.save_active_session();
                }

                if toggle_ctx_settings {
                    self.show_context_settings = !self.show_context_settings;
                }

                if toggle_vault_settings {
                    self.show_vault_modal = !self.show_vault_modal;
                }
            });

        // 2. LEFT SIDEBAR (SESSION MANAGER — Linear alignment, zero bouncing margins)
        if self.show_sidebar {
            egui::SidePanel::left("sessions_sidebar")
                .frame(
                    Frame::none()
                        .fill(Color32::from_rgb(11, 13, 19))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(24, 29, 41)))
                        .inner_margin(Margin::symmetric(10.0, 12.0)),
                )
                .default_width(260.0)
                .width_range(220.0..=360.0)
                .show(ctx, |ui| {
                    let side_w = ui.available_width();

                    // New Session button
                    let new_btn = egui::Button::new(
                        RichText::new("＋  New Session")
                            .color(Color32::from_rgb(248, 250, 252))
                            .strong()
                            .size(12.5),
                    )
                    .fill(Color32::from_rgb(37, 99, 235))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(59, 130, 246)))
                    .rounding(Rounding::same(7.0))
                    .min_size(Vec2::new(side_w, 32.0));

                    if ui.add(new_btn).clicked() {
                        self.new_chat();
                    }

                    ui.add_space(8.0);

                    // Search box
                    Frame::none()
                        .fill(Color32::from_rgb(16, 20, 29))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(28, 35, 50)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.session_filter)
                                    .hint_text("🔍  Search sessions...")
                                    .desired_width(side_w)
                                    .font(egui::TextStyle::Body)
                                    .frame(false),
                            );
                        });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let filter = self.session_filter.to_lowercase();
                            let active_id = self.active_session_id.clone();
                            let mut to_switch = None;
                            let mut to_delete = None;

                            let total_w = ui.available_width();

                            for sess in &self.sessions {
                                if !filter.is_empty()
                                    && !sess.title.to_lowercase().contains(&filter)
                                {
                                    continue;
                                }

                                let is_active = Some(&sess.id) == active_id.as_ref();
                                let card_size = Vec2::new(total_w, 36.0);
                                let (rect, resp) =
                                    ui.allocate_exact_size(card_size, egui::Sense::click());

                                let is_hovered = resp.hovered();

                                let bg_color = if is_active {
                                    Color32::from_rgb(26, 34, 52)
                                } else if is_hovered {
                                    Color32::from_rgb(20, 25, 36)
                                } else {
                                    Color32::from_rgb(14, 17, 25)
                                };

                                let border_stroke = if is_active {
                                    Stroke::new(1.0, Color32::from_rgb(59, 130, 246))
                                } else if is_hovered {
                                    Stroke::new(1.0, Color32::from_rgb(38, 48, 70))
                                } else {
                                    Stroke::new(1.0, Color32::from_rgb(24, 29, 41))
                                };

                                let painter = ui.painter();
                                painter.rect(rect, Rounding::same(6.0), bg_color, border_stroke);

                                let icon = if sess.title.to_lowercase().contains("deep") {
                                    "🔬"
                                } else if sess.title.to_lowercase().contains("cargo")
                                    || sess.title.to_lowercase().contains("run")
                                {
                                    "💻"
                                } else {
                                    "💬"
                                };

                                // Draw icon
                                let icon_pos = Pos2::new(rect.left() + 9.0, rect.center().y - 7.0);
                                painter.text(
                                    icon_pos,
                                    egui::Align2::LEFT_CENTER,
                                    icon,
                                    egui::FontId::new(12.0, egui::FontFamily::Proportional),
                                    Color32::from_rgb(148, 163, 184),
                                );

                                // Title text with strict boundary
                                let text_pos = Pos2::new(rect.left() + 29.0, rect.center().y);
                                let text_color = if is_active {
                                    Color32::from_rgb(248, 250, 252)
                                } else if is_hovered {
                                    Color32::from_rgb(203, 213, 225)
                                } else {
                                    Color32::from_rgb(148, 163, 184)
                                };

                                let max_title_chars = ((total_w - 60.0) / 7.5).max(10.0) as usize;
                                let title_text = if sess.title.chars().count() > max_title_chars {
                                    format!(
                                        "{}...",
                                        sess.title
                                            .chars()
                                            .take(max_title_chars.saturating_sub(2))
                                            .collect::<String>()
                                    )
                                } else {
                                    sess.title.clone()
                                };

                                painter.text(
                                    text_pos,
                                    egui::Align2::LEFT_CENTER,
                                    title_text,
                                    egui::FontId::new(12.0, egui::FontFamily::Proportional),
                                    text_color,
                                );

                                // Delete button: fixed 18px from right edge
                                let del_rect = Rect::from_center_size(
                                    Pos2::new(rect.right() - 15.0, rect.center().y),
                                    Vec2::new(18.0, 18.0),
                                );
                                let del_resp = ui.interact(
                                    del_rect,
                                    ui.id().with(&sess.id).with("del"),
                                    egui::Sense::click(),
                                );
                                if del_resp.hovered() {
                                    painter.rect_filled(
                                        del_rect,
                                        Rounding::same(4.0),
                                        Color32::from_rgb(45, 20, 25),
                                    );
                                    painter.text(
                                        del_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "✕",
                                        egui::FontId::new(10.0, egui::FontFamily::Proportional),
                                        Color32::from_rgb(248, 113, 113),
                                    );
                                } else if is_hovered || is_active {
                                    painter.text(
                                        del_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "✕",
                                        egui::FontId::new(10.0, egui::FontFamily::Proportional),
                                        Color32::from_rgb(100, 116, 139),
                                    );
                                }

                                if del_resp.clicked() {
                                    to_delete = Some(sess.id.clone());
                                } else if resp.clicked() {
                                    to_switch = Some(sess.id.clone());
                                }

                                ui.add_space(3.0);
                            }

                            if let Some(id) = to_switch {
                                self.switch_session(id);
                            }
                            if let Some(id) = to_delete {
                                self.session_manager.delete_session(&id);
                                self.sessions.retain(|s| s.id != id);
                                if self.active_session_id.as_deref() == Some(&id) {
                                    self.active_session_id =
                                        self.sessions.first().map(|s| s.id.clone());
                                }
                            }
                        });
                });
        }

        // 3. BOTTOM FLOATING INPUT CONTAINER (Docked, capped height with internal scroll)
        egui::TopBottomPanel::bottom("bottom_input_v3")
            .resizable(false)
            .max_height(185.0)
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(13, 15, 20))
                    .inner_margin(Margin::symmetric(16.0, 6.0)),
            )
            .show(ctx, |ui| {
                Frame::none()
                    .fill(Color32::from_rgb(16, 20, 28))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(28, 36, 50)))
                    .rounding(Rounding::same(9.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 1.5),
                        blur: 8.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(45),
                    })
                    .inner_margin(Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        // Header row: Status & Stop generation
                        ui.horizontal(|ui| {
                            if self.is_generating {
                                if ui.add(
                                    egui::Button::new(RichText::new("⏹  Stop").color(Color32::from_rgb(255, 100, 100)).strong().size(11.0))
                                        .fill(Color32::from_rgb(45, 20, 25))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(220, 38, 38)))
                                        .rounding(Rounding::same(4.0)),
                                ).clicked() {
                                    self.cancel_current_generation();
                                }
                                ui.add_space(6.0);
                            }

                            ui.label(RichText::new(&self.status_message).color(Color32::from_rgb(148, 163, 184)).size(11.0));
                        });

                        ui.add_space(2.0);

                        if !self.staged_images.is_empty() {
                            let mut remove_idx = None;
                            ui.horizontal_wrapped(|ui| {
                                for (idx, att) in self.staged_images.iter().enumerate() {
                                    Frame::none()
                                        .fill(Color32::from_rgb(14, 18, 28))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(59, 130, 246)))
                                        .rounding(Rounding::same(6.0))
                                        .inner_margin(Margin::same(4.0))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                if let Some(bytes) = att.raw_bytes() {
                                                    ui.add(
                                                        egui::Image::from_bytes(format!("bytes://staged_{}.png", att.id), bytes)
                                                            .max_height(40.0)
                                                            .max_width(60.0)
                                                            .rounding(Rounding::same(4.0))
                                                    );
                                                }
                                                ui.vertical(|ui| {
                                                    ui.label(RichText::new(format!("{}x{}", att.width, att.height))
                                                        .size(10.0)
                                                        .strong()
                                                        .color(Color32::from_rgb(226, 232, 240)));
                                                    ui.label(RichText::new(format!("{} KB", att.byte_size / 1024))
                                                        .size(9.0)
                                                        .color(Color32::from_rgb(148, 163, 184)));
                                                });
                                                if ui.button(RichText::new("✕").color(Color32::from_rgb(239, 68, 68)).strong().size(11.0)).on_hover_text("Remove image").clicked() {
                                                    remove_idx = Some(idx);
                                                }
                                            });
                                        });
                                    ui.add_space(4.0);
                                }
                            });
                            if let Some(idx) = remove_idx {
                                self.staged_images.remove(idx);
                            }
                            ui.add_space(2.0);
                        }

                        // Scrollable multiline TextEdit capped at ~100px with smooth vertical scroll
                        let input_response = egui::ScrollArea::vertical()
                            .id_salt("bottom_input_text_scroll")
                            .max_height(100.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.input_text)
                                        .hint_text("Ask anything, or run /deep, /calc, /run... (Ctrl+Enter to send)")
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY)
                                        .font(egui::TextStyle::Body)
                                        .frame(false),
                                )
                            })
                            .inner;

                        // Check Ctrl+V specifically when text edit has focus or globally
                        if input_response.has_focus() && ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::V)) {
                            if let Ok(att) = ChatImageAttachment::from_clipboard() {
                                let dims = format!("{}x{}", att.width, att.height);
                                let kb = att.byte_size / 1024;
                                self.status_message = format!("Attached clipboard image ({} px, {} KB)", dims, kb);
                                self.staged_images.push(att);
                            }
                        }

                        if input_response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl) {
                            self.send_message();
                        }

                        ui.add_space(4.0);

                        // Footer row: Quick Action Pills + Send Button
                        ui.horizontal(|ui| {
                            // Quick command pills
                            let pill = |ui: &mut egui::Ui, text: &str, border: Color32| -> egui::Response {
                                ui.add(
                                    egui::Button::new(RichText::new(text).color(Color32::from_rgb(226, 232, 240)).size(11.5))
                                        .fill(Color32::from_rgb(24, 28, 40))
                                        .stroke(Stroke::new(1.0, border))
                                        .rounding(Rounding::same(6.0)),
                                )
                            };

                            if pill(ui, "🔬 /deep", Color32::from_rgb(99, 102, 241)).clicked() {
                                self.input_text = "/deep ".to_string();
                            }
                            if pill(ui, "⚡ /calc", Color32::from_rgb(251, 191, 36)).clicked() {
                                self.input_text = "/calc ".to_string();
                            }
                            if pill(ui, "💻 /run", Color32::from_rgb(52, 211, 153)).clicked() {
                                self.input_text = "/run ".to_string();
                            }
                            if pill(ui, "📎 Paste Image", Color32::from_rgb(168, 85, 247)).on_hover_text("Paste image from clipboard (Ctrl+V) or drag & drop files").clicked() {
                                self.paste_image_from_clipboard();
                            }
                            if pill(ui, "🗑 Clear", Color32::from_rgb(75, 85, 99)).clicked() {
                                if let Some(sess) = self.active_session_mut() {
                                    sess.messages.clear();
                                }
                                self.save_active_session();
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let can_send = (!self.input_text.trim().is_empty() || !self.staged_images.is_empty()) && !self.is_generating;
                                let send_btn = egui::Button::new(
                                    RichText::new("Send  ↵")
                                        .color(if can_send { Color32::WHITE } else { Color32::from_rgb(120, 130, 150) })
                                        .strong()
                                        .size(13.0),
                                )
                                .fill(if can_send { Color32::from_rgb(37, 99, 235) } else { Color32::from_rgb(28, 34, 48) })
                                .stroke(Stroke::new(1.0, if can_send { Color32::from_rgb(59, 130, 246) } else { Color32::from_rgb(40, 48, 64) }))
                                .rounding(Rounding::same(8.0))
                                .min_size(Vec2::new(90.0, 28.0));

                                if ui.add(send_btn).clicked() {
                                    self.send_message();
                                }
                            });
                        });
                    });
            });

        // 4. RIGHT PROCESS & TERMINAL PANEL
        if self.show_process_panel {
            egui::SidePanel::right("process_panel")
                .frame(
                    Frame::none()
                        .fill(Color32::from_rgb(10, 12, 18))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(24, 30, 44)))
                        .inner_margin(Margin::symmetric(12.0, 10.0)),
                )
                .default_width(380.0)
                .width_range(300.0..=600.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("◈  Logs & Runtime").color(Color32::from_rgb(96, 165, 250)).strong().size(12.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(RichText::new("✕").color(Color32::GRAY)).clicked() {
                                self.show_process_panel = false;
                            }
                            if ui.button(RichText::new("Clear").size(11.0)).clicked() {
                                self.process_logs.clear();
                            }
                            if let Some(payload) = &self.last_request_payload {
                                if ui.button(RichText::new("📋 Copy JSON").size(11.0)).on_hover_text("Copy last request payload sent to LM Studio").clicked() {
                                    ui.output_mut(|o| o.copied_text = payload.clone());
                                    self.status_message = "Copied last request JSON payload to clipboard!".to_string();
                                }
                            }
                        });
                    });
                    ui.separator();
                    ui.add_space(4.0);

                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        if self.process_logs.is_empty() {
                            ui.label(RichText::new("No subprocess logs yet.\nExecute /run, /calc, or /deep to see live stderr/stdout.")
                                .color(Color32::from_rgb(100, 116, 139))
                                .italics()
                                .size(12.0));
                        } else {
                            for log in &self.process_logs {
                                Frame::none()
                                    .fill(Color32::from_rgb(14, 17, 24))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(26, 32, 46)))
                                    .rounding(Rounding::same(6.0))
                                    .inner_margin(Margin::same(8.0))
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(log).monospace().color(Color32::from_rgb(167, 243, 208)).size(11.5));
                                    });
                                ui.add_space(6.0);
                            }
                        }
                    });
                });
        }

        // 5. MAIN CENTRAL CHAT AREA
        egui::CentralPanel::default()
            .frame(Frame::none().fill(Color32::from_rgb(13, 15, 20)).inner_margin(Margin::symmetric(24.0, 12.0)))
            .show(ctx, |ui| {
                let has_messages = self.active_session().map(|s| !s.messages.is_empty()).unwrap_or(false);

                if !has_messages && !self.is_generating {
                    // Empty state: render modern welcome hero with action cards in scrollable view
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(action) = render_welcome_hero(ui, &self.selected_model, &self.server_url) {
                                self.input_text = action;
                            }
                        });
                } else {
                    let mut manual_tool_action = None;
                    let mut pending_rollback = None;
                    let mut next_confirm_rollback = self.confirm_rollback.clone();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if let Some(sess) = self.active_session() {
                                let total_messages = sess.messages.len();
                                for (idx, msg) in sess.messages.iter().enumerate() {
                                    ui.push_id(&msg.id, |ui| {
                                        match msg.role {
                                            MessageRole::User => {
                                                // Suppress internal system nudges/instructions from rendering as user bubbles
                                                if msg.content.starts_with("[INSTRUCTION]")
                                                    || msg.content.starts_with("[DEEP RESEARCH")
                                                    || msg.content.starts_with("[FINAL DOSSIER")
                                                {
                                                    return;
                                                }

                                                ui.horizontal(|ui| {
                                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                                        Frame::none()
                                                            .fill(Color32::from_rgb(26, 38, 58))
                                                            .stroke(Stroke::new(1.0, Color32::from_rgb(45, 68, 104)))
                                                            .rounding(Rounding::same(10.0))
                                                            .inner_margin(Margin::same(12.0))
                                                            .show(ui, |ui| {
                                                                ui.vertical(|ui| {
                                                                    ui.horizontal(|ui| {
                                                                        ui.label(RichText::new("👤 You").color(Color32::from_rgb(147, 197, 253)).strong().size(12.0));
                                                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                                            let is_confirm_edit = self.confirm_rollback.as_deref() == Some(&format!("edit_{}", msg.id));
                                                                            let edit_text = if is_confirm_edit { "⚠️ Confirm Edit?" } else { "✏️ Edit" };
                                                                            let edit_btn = ui.add(
                                                                                egui::Button::new(RichText::new(edit_text).color(if is_confirm_edit { Color32::from_rgb(252, 165, 165) } else { Color32::from_rgb(148, 163, 184) }).size(11.0))
                                                                                    .fill(if is_confirm_edit { Color32::from_rgb(50, 20, 25) } else { Color32::from_rgb(20, 28, 42) })
                                                                                    .stroke(Stroke::new(1.0, if is_confirm_edit { Color32::from_rgb(239, 68, 68) } else { Color32::from_rgb(38, 52, 78) }))
                                                                                    .rounding(Rounding::same(4.0)),
                                                                            );
                                                                            if edit_btn.clicked() {
                                                                                if is_confirm_edit || idx + 1 >= total_messages {
                                                                                    next_confirm_rollback = None;
                                                                                    pending_rollback = Some(RollbackTarget::EditUser(msg.id.clone()));
                                                                                } else {
                                                                                    next_confirm_rollback = Some(format!("edit_{}", msg.id));
                                                                                }
                                                                            }

                                                                            ui.add_space(4.0);

                                                                            let is_confirm_retry = self.confirm_rollback.as_deref() == Some(&format!("retry_{}", msg.id));
                                                                            let retry_text = if is_confirm_retry { "⚠️ Confirm Retry?" } else { "🔄 Retry" };
                                                                            let retry_btn = ui.add(
                                                                                egui::Button::new(RichText::new(retry_text).color(if is_confirm_retry { Color32::from_rgb(252, 165, 165) } else { Color32::from_rgb(148, 163, 184) }).size(11.0))
                                                                                    .fill(if is_confirm_retry { Color32::from_rgb(50, 20, 25) } else { Color32::from_rgb(20, 28, 42) })
                                                                                    .stroke(Stroke::new(1.0, if is_confirm_retry { Color32::from_rgb(239, 68, 68) } else { Color32::from_rgb(38, 52, 78) }))
                                                                                    .rounding(Rounding::same(4.0)),
                                                                            );
                                                                            if retry_btn.clicked() {
                                                                                if is_confirm_retry || idx + 1 >= total_messages {
                                                                                    next_confirm_rollback = None;
                                                                                    pending_rollback = Some(RollbackTarget::RetryUser(msg.id.clone()));
                                                                                } else {
                                                                                    next_confirm_rollback = Some(format!("retry_{}", msg.id));
                                                                                }
                                                                            }
                                                                        });
                                                                    });
                                                                    ui.add_space(4.0);
                                                                    if !msg.images.is_empty() {
                                                                        ui.horizontal_wrapped(|ui| {
                                                                            for (img_i, att) in msg.images.iter().enumerate() {
                                                                                if let Some(bytes) = att.raw_bytes() {
                                                                                    Frame::none()
                                                                                        .fill(Color32::from_rgb(12, 16, 24))
                                                                                        .stroke(Stroke::new(1.0, Color32::from_rgb(45, 60, 90)))
                                                                                        .rounding(Rounding::same(8.0))
                                                                                        .inner_margin(Margin::same(4.0))
                                                                                        .show(ui, |ui| {
                                                                                            ui.add(
                                                                                                egui::Image::from_bytes(format!("bytes://user_msg_{}_{}.png", msg.id, img_i), bytes)
                                                                                                    .max_width(520.0)
                                                                                                    .max_height(400.0)
                                                                                                    .rounding(Rounding::same(6.0))
                                                                                            );
                                                                                        });
                                                                                    ui.add_space(6.0);
                                                                                }
                                                                            }
                                                                        });
                                                                        ui.add_space(6.0);
                                                                    }
                                                                    ui.push_id(("user_body", &msg.id), |ui| {
                                                                        render_message_content(ui, &msg.content);
                                                                    });
                                                                });
                                                            });
                                                    });
                                                });
                                                ui.add_space(10.0);
                                            }
                                            MessageRole::Assistant => {
                                                let bubble_w = (ui.available_width() - 4.0).max(100.0);
                                                Frame::none()
                                                    .fill(Color32::from_rgb(17, 21, 30))
                                                    .stroke(Stroke::new(1.0, Color32::from_rgb(30, 38, 54)))
                                                    .rounding(Rounding::same(8.0))
                                                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 1.5),
                        blur: 8.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(35),
                    })
                                                    .inner_margin(Margin::same(14.0))
                                                    .show(ui, |ui| {
                                                        ui.set_max_width(bubble_w - 28.0);
                                                        ui.horizontal(|ui| {
                                                            ui.label(RichText::new("◈  Assistant").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                                let copy_btn = ui.add(
                                                                    egui::Button::new(RichText::new("📋 Copy").color(Color32::from_rgb(148, 163, 184)).size(11.0))
                                                                        .fill(Color32::from_rgb(26, 32, 46))
                                                                        .stroke(Stroke::new(1.0, Color32::from_rgb(38, 48, 68)))
                                                                        .rounding(Rounding::same(5.0)),
                                                                );
                                                                if copy_btn.clicked() {
                                                                    ui.output_mut(|o| o.copied_text = msg.content.clone());
                                                                }

                                                                ui.add_space(4.0);

                                                                let is_confirm_regen = self.confirm_rollback.as_deref() == Some(&format!("regen_{}", msg.id));
                                                                let regen_text = if is_confirm_regen { "⚠️ Confirm?" } else { "🔄 Regenerate" };
                                                                let regen_btn = ui.add(
                                                                    egui::Button::new(RichText::new(regen_text).color(if is_confirm_regen { Color32::from_rgb(252, 165, 165) } else { Color32::from_rgb(148, 163, 184) }).size(11.0))
                                                                        .fill(if is_confirm_regen { Color32::from_rgb(50, 20, 25) } else { Color32::from_rgb(26, 32, 46) })
                                                                        .stroke(Stroke::new(1.0, if is_confirm_regen { Color32::from_rgb(239, 68, 68) } else { Color32::from_rgb(38, 48, 68) }))
                                                                        .rounding(Rounding::same(5.0)),
                                                                );
                                                                if regen_btn.clicked() {
                                                                    if is_confirm_regen || idx + 1 >= total_messages {
                                                                        next_confirm_rollback = None;
                                                                        pending_rollback = Some(RollbackTarget::RegenerateAssistant(msg.id.clone()));
                                                                    } else {
                                                                        next_confirm_rollback = Some(format!("regen_{}", msg.id));
                                                                    }
                                                                }

                                                                if idx + 1 < total_messages {
                                                                    ui.add_space(4.0);
                                                                    let is_confirm_rewind = self.confirm_rollback.as_deref() == Some(&format!("rewind_{}", msg.id));
                                                                    let rewind_text = if is_confirm_rewind { "⚠️ Confirm?" } else { "⏪ Rewind to here" };
                                                                    let rewind_btn = ui.add(
                                                                        egui::Button::new(RichText::new(rewind_text).color(if is_confirm_rewind { Color32::from_rgb(252, 165, 165) } else { Color32::from_rgb(148, 163, 184) }).size(11.0))
                                                                            .fill(if is_confirm_rewind { Color32::from_rgb(50, 20, 25) } else { Color32::from_rgb(26, 32, 46) })
                                                                            .stroke(Stroke::new(1.0, if is_confirm_rewind { Color32::from_rgb(239, 68, 68) } else { Color32::from_rgb(38, 48, 68) }))
                                                                            .rounding(Rounding::same(5.0)),
                                                                    );
                                                                    if rewind_btn.clicked() {
                                                                        if is_confirm_rewind {
                                                                            next_confirm_rollback = None;
                                                                            pending_rollback = Some(RollbackTarget::RewindAssistant(msg.id.clone()));
                                                                        } else {
                                                                            next_confirm_rollback = Some(format!("rewind_{}", msg.id));
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        });
                                                        ui.add_space(6.0);

                                                        // Collapsible thinking section
                                                        if let Some(th) = &msg.thinking {
                                                            egui::CollapsingHeader::new(
                                                                RichText::new("🧠 Chain of Thought")
                                                                    .color(Color32::from_rgb(245, 158, 11))
                                                                    .size(12.0)
                                                            )
                                                            .id_salt("think_hdr")
                                                            .default_open(false)
                                                            .show(ui, |ui| {
                                                                Frame::none()
                                                                    .fill(Color32::from_rgb(14, 17, 24))
                                                                    .stroke(Stroke::new(1.0, Color32::from_rgb(26, 32, 46)))
                                                                    .rounding(Rounding::same(6.0))
                                                                    .inner_margin(Margin::same(8.0))
                                                                    .show(ui, |ui| {
                                                                        ui.label(RichText::new(th).color(Color32::from_rgb(148, 163, 184)).monospace().size(11.5));
                                                                    });
                                                            });
                                                            ui.add_space(6.0);
                                                        }

                                                        // Render tool calls
                                                        for (idx, tc) in msg.tool_calls.iter().enumerate() {
                                                            ui.push_id(idx, |ui| {
                                                                let (status_label, color) = match tc.status {
                                                                    ToolStatus::Pending => ("[Pending]", Color32::from_rgb(234, 179, 8)),
                                                                    ToolStatus::Running => ("[Running...]", Color32::from_rgb(249, 115, 22)),
                                                                    ToolStatus::Success => ("[Executed]", Color32::from_rgb(52, 211, 153)),
                                                                    ToolStatus::Failed => ("[Failed]", Color32::from_rgb(239, 68, 68)),
                                                                    ToolStatus::Cancelled => ("[Cancelled]", Color32::GRAY),
                                                                };

                                                                Frame::none()
                                                                    .fill(Color32::from_rgb(14, 18, 26))
                                                                    .stroke(Stroke::new(1.0, color))
                                                                    .rounding(Rounding::same(6.0))
                                                                    .inner_margin(Margin::same(10.0))
                                                                    .show(ui, |ui| {
                                                                        ui.label(RichText::new(format!("{} Tool Call: {}", status_label, tc.name)).color(color).strong().size(12.0));

                                                                        let lang = if tc.name == "execute_python" {
                                                                            "python"
                                                                        } else if tc.name == "web_search" || tc.name == "search" {
                                                                            "text"
                                                                        } else if tc.name == "fetch_page" || tc.name == "fetch" {
                                                                            "text"
                                                                        } else {
                                                                            "powershell"
                                                                        };
                                                                        let code_body = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                                                                            if let Some(c) = v.get("code").and_then(|s| s.as_str()) {
                                                                                c.to_string()
                                                                            } else if let Some(cmd) = v.get("cmd").and_then(|s| s.as_str()) {
                                                                                cmd.to_string()
                                                                            } else if let Some(q) = v.get("query").or_else(|| v.get("q")).and_then(|s| s.as_str()) {
                                                                                format!("Search query: {}", q)
                                                                            } else if let Some(u) = v.get("url").and_then(|s| s.as_str()) {
                                                                                format!("Fetch URL: {}", u)
                                                                            } else {
                                                                                tc.arguments.clone()
                                                                            }
                                                                        } else {
                                                                            tc.arguments.clone()
                                                                        };

                                                                        ui.push_id(("tc_code_body", idx, &tc.id), |ui| {
                                                                            render_message_content(ui, &format!("```{}\n{}\n```", lang, code_body));
                                                                        });

                                                                        if let Some(out) = &tc.output {
                                                                            ui.add_space(4.0);
                                                                            let line_count = out.lines().count();
                                                                            let is_large = line_count > 6 || out.len() > 300;
                                                                            if is_large {
                                                                                egui::CollapsingHeader::new(
                                                                                    RichText::new(format!("📄 Execution Output ({} lines, {} B)", line_count, out.len()))
                                                                                        .color(Color32::from_rgb(148, 163, 184))
                                                                                        .size(11.5)
                                                                                )
                                                                                .id_salt(("tc_out_hdr", idx, &tc.id))
                                                                                .default_open(false)
                                                                                .show(ui, |ui| {
                                                                                    ui.push_id(("tc_out_body", idx, &tc.id), |ui| {
                                                                                        render_message_content(ui, &format!("```text\n{}\n```", out));
                                                                                    });
                                                                                });
                                                                            } else {
                                                                                ui.label(RichText::new("Execution Output:").color(Color32::from_rgb(148, 163, 184)).size(11.5));
                                                                                ui.push_id(("tc_out_body", idx, &tc.id), |ui| {
                                                                                    render_message_content(ui, &format!("```text\n{}\n```", out));
                                                                                });
                                                                            }
                                                                        }
                                                                    });
                                                                ui.add_space(8.0);
                                                            });
                                                        }

                                                        let clean_content = strip_tool_tags(&msg.content);
                                                        if !clean_content.trim().is_empty() {
                                                            ui.push_id(("assistant_content_body", &msg.id), |ui| {
                                                                if let Some(act) = render_message_content(ui, &clean_content) {
                                                                    manual_tool_action = Some(act);
                                                                }
                                                            });
                                                        } else if msg.tool_calls.is_empty() {
                                                            ui.label(RichText::new("(Empty response)").color(Color32::GRAY).italics());
                                                        }
                                                    });
                                                ui.add_space(10.0);
                                            }
                                            MessageRole::Tool => {}
                                            MessageRole::System => {}
                                        }
                                    });
                                }
                            }

                            // Live generation stream container
                            if self.is_generating {
                                ui.push_id("live_gen_container", |ui| {
                                    let bubble_w = (ui.available_width() - 4.0).max(100.0);
                                    Frame::none()
                                        .fill(Color32::from_rgb(17, 21, 30))
                                        .stroke(Stroke::new(1.0, Color32::from_rgb(59, 130, 246)))
                                        .rounding(Rounding::same(8.0))
                                        .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 1.5),
                        blur: 8.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(40),
                    })
                                        .inner_margin(Margin::same(14.0))
                                        .show(ui, |ui| {
                                            ui.set_max_width(bubble_w - 28.0);
                                            ui.horizontal(|ui| {
                                                ui.spinner();
                                                ui.label(RichText::new("Silo Assistant (Generating...)").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                            });
                                            ui.add_space(6.0);

                                            if !self.live_thinking.is_empty() {
                                                egui::CollapsingHeader::new(RichText::new("🧠 Thinking live...").color(Color32::from_rgb(245, 158, 11)).size(12.0))
                                                    .id_salt("live_think_hdr")
                                                    .default_open(true)
                                                    .show(ui, |ui| {
                                                        Frame::none()
                                                            .fill(Color32::from_rgb(14, 17, 24))
                                                            .stroke(Stroke::new(1.0, Color32::from_rgb(26, 32, 46)))
                                                            .rounding(Rounding::same(6.0))
                                                            .inner_margin(Margin::same(8.0))
                                                            .show(ui, |ui| {
                                                                ui.label(RichText::new(&self.live_thinking).color(Color32::from_rgb(148, 163, 184)).monospace().size(11.5));
                                                            });
                                                    });
                                                ui.add_space(6.0);
                                            }

                                            if !self.live_tool_draft.is_empty() {
                                                Frame::none()
                                                    .fill(Color32::from_rgb(24, 20, 14))
                                                    .stroke(Stroke::new(1.0, Color32::from_rgb(245, 158, 11)))
                                                    .rounding(Rounding::same(6.0))
                                                    .inner_margin(Margin::same(8.0))
                                                    .show(ui, |ui| {
                                                        ui.label(RichText::new("Drafting Tool Call...").color(Color32::from_rgb(251, 191, 36)).strong().size(12.0));
                                                        ui.push_id("live_tool_draft_body", |ui| {
                                                            render_message_content(ui, &format!("```python\n{}\n```", &self.live_tool_draft));
                                                        });
                                                    });
                                                ui.add_space(6.0);
                                            }

                                            if !self.live_content.is_empty() {
                                                let clean_live = strip_tool_tags(&self.live_content);
                                                if !clean_live.is_empty() {
                                                    ui.push_id("live_content_body", |ui| {
                                                        render_message_content(ui, &clean_live);
                                                    });
                                                }
                                            }
                                        });
                                    ui.add_space(10.0);
                                });
                            }
                        });

                    if let Some((tool_name, code_or_cmd)) = manual_tool_action {
                        self.dispatch_manual_tool_execution(tool_name, code_or_cmd);
                    }

                    self.confirm_rollback = next_confirm_rollback;

                    if let Some(target) = pending_rollback {
                        self.execute_rollback(target);
                    }
                }
            });

        // 5.5 VAULT SECURITY & ENCRYPTION MODAL
        if self.show_vault_modal {
            let mut open = true;
            egui::Window::new("◈ Silo Vault Security & Encryption")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::new(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(RichText::new("CRYPTOGRAPHIC VAULT CONFIGURATION").color(Color32::from_rgb(56, 189, 248)).strong().size(11.5));
                    ui.add_space(6.0);
                    ui.label(RichText::new("Sessions are serialized via binary Postcard and encrypted with ChaCha20-Poly1305 AEAD. Cryptographic nonces and keys are sourced from hardware CPU intrinsics via shake-entropy.").color(Color32::from_rgb(148, 163, 184)).size(12.0));
                    ui.add_space(12.0);

                    let is_protected = self.vault_manager.is_password_protected();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Current Mode:").strong());
                        if is_protected {
                            ui.label(RichText::new("🔐 Master Password Protected (Argon2id KEK)").color(Color32::from_rgb(74, 222, 128)).strong());
                        } else {
                            ui.label(RichText::new("🔑 Transparent Machine Key (~/.silo/silo.key)").color(Color32::from_rgb(56, 189, 248)).strong());
                        }
                    });

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(14.0);

                    if is_protected {
                        ui.label(RichText::new("Switch back to Transparent Machine Key:").size(12.5).color(Color32::from_rgb(226, 232, 240)));
                        ui.label(RichText::new("Removes the password prompt on startup. Sessions remain encrypted on disk.").size(11.5).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(8.0);
                        if let Some(key) = &self.session_manager.vault_key {
                            if ui.button("Revert to Machine Key (Remove Password)").clicked() {
                                if let Err(e) = self.vault_manager.remove_master_password(key) {
                                    self.vault_status_msg = Some(format!("Error: {}", e));
                                } else {
                                    self.vault_status_msg = Some("Reverted to transparent Machine Key.".to_string());
                                }
                            }
                        }
                    } else {
                        ui.label(RichText::new("Set Master Password for Silo:").size(12.5).color(Color32::from_rgb(226, 232, 240)));
                        ui.label(RichText::new("Encrypts the master vault key with Argon2id. You will be prompted for this password when launching Silo.").size(11.5).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(10.0);

                        ui.horizontal(|ui| {
                            ui.label("New Password:     ");
                            ui.add(egui::TextEdit::singleline(&mut self.new_password_input).password(true));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Confirm Password: ");
                            ui.add(egui::TextEdit::singleline(&mut self.confirm_password_input).password(true));
                        });

                        ui.add_space(10.0);
                        if ui.button("🔒  Enable Master Password Protection").clicked() {
                            if self.new_password_input.is_empty() {
                                self.vault_status_msg = Some("Password cannot be empty.".to_string());
                            } else if self.new_password_input != self.confirm_password_input {
                                self.vault_status_msg = Some("Passwords do not match.".to_string());
                            } else if let Some(key) = &self.session_manager.vault_key {
                                match self.vault_manager.set_master_password(key, &self.new_password_input) {
                                    Ok(_) => {
                                        self.new_password_input.clear();
                                        self.confirm_password_input.clear();
                                        self.vault_status_msg = Some("Master password enabled successfully!".to_string());
                                    }
                                    Err(e) => {
                                        self.vault_status_msg = Some(format!("Error: {}", e));
                                    }
                                }
                            }
                        }
                    }

                    if let Some(msg) = &self.vault_status_msg {
                        ui.add_space(10.0);
                        ui.label(RichText::new(msg).color(Color32::from_rgb(56, 189, 248)).size(12.0));
                    }
                });
            if !open {
                self.show_vault_modal = false;
                self.vault_status_msg = None;
            }
        }

        // 5.6 INFERENCE SERVER & HOST MODAL
        if self.show_server_modal {
            let mut open = true;
            let mut close_clicked = false;
            egui::Window::new("◈ Inference Server & API Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(520.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::new(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(RichText::new("OPENAI-COMPATIBLE INFERENCE ENDPOINT").color(Color32::from_rgb(56, 189, 248)).strong().size(11.5));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Connect to local engines (LM Studio, llama.cpp, Ollama, vLLM) or remote OpenAI-compatible cloud providers.").color(Color32::from_rgb(148, 163, 184)).size(12.0));
                    ui.add_space(10.0);

                    ui.label(RichText::new("QUICK ENDPOINT PRESETS:").color(Color32::from_rgb(56, 189, 248)).strong().size(11.0));
                    let mut connect_trigger = false;
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("LM Studio (:1234)").clicked() {
                            self.server_url = "http://127.0.0.1:1234".to_string();
                            connect_trigger = true;
                        }
                        if ui.button("llama.cpp (:8080)").clicked() {
                            self.server_url = "http://127.0.0.1:8080".to_string();
                            connect_trigger = true;
                        }
                        if ui.button("Ollama (:11434)").clicked() {
                            self.server_url = "http://127.0.0.1:11434".to_string();
                            connect_trigger = true;
                        }
                        if ui.button("vLLM (:8000)").clicked() {
                            self.server_url = "http://127.0.0.1:8000".to_string();
                            connect_trigger = true;
                        }
                        if ui.button("OpenRouter").clicked() {
                            self.server_url = "https://openrouter.ai/api/v1".to_string();
                        }
                    });
                    ui.add_space(10.0);

                    ui.label(RichText::new("Server Base URL:").strong().size(12.0));
                    ui.add(egui::TextEdit::singleline(&mut self.server_url).desired_width(f32::INFINITY).hint_text("http://127.0.0.1:1234"));
                    ui.add_space(6.0);

                    ui.label(RichText::new("Active Model Identifier (Text Override):").strong().size(12.0));
                    ui.add(egui::TextEdit::singleline(&mut self.selected_model).desired_width(f32::INFINITY).hint_text("e.g. glm-4.7-flash, anthropic/claude-3.5-sonnet, deepseek-chat"));
                    ui.add_space(6.0);

                    ui.label(RichText::new("API Key / Bearer Token (Optional):").strong().size(12.0));
                    ui.add(egui::TextEdit::singleline(&mut self.api_key).desired_width(f32::INFINITY).password(true).hint_text("sk-... (leave empty for local engines)"));
                    ui.add_space(12.0);

                    if let Some(msg) = &self.server_status_msg {
                        let is_err = msg.contains("0 model") || msg.contains("failed") || msg.contains("unreachable");
                        ui.label(RichText::new(msg).color(if is_err { Color32::from_rgb(248, 113, 113) } else { Color32::from_rgb(52, 211, 153) }).size(12.0));
                        ui.add_space(8.0);
                    }

                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("🔄 Connect & Refresh Models").strong().color(Color32::WHITE)).clicked() || connect_trigger {
                            let tx = self.models_tx.clone();
                            let url = self.server_url.clone();
                            let key = if self.api_key.trim().is_empty() { None } else { Some(self.api_key.trim().to_string()) };
                            self.server_status_msg = Some("Querying host models...".to_string());
                            self.tokio_handle.spawn(async move {
                                let m = fetch_available_models(&url, key.as_deref()).await;
                                let _ = tx.send(m);
                            });
                        }

                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });
            if !open || close_clicked {
                self.show_server_modal = false;
            }
        }

        // 6. CONTEXT & PRIVACY SETTINGS MODAL
        if self.show_context_settings {
            let mut open = true;
            let mut save_needed = false;

            egui::Window::new("◈ Dialogue Context & Privacy Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .default_height(600.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::new(0.0, 0.0))
                .show(ctx, |ui| {
                    if let Some(sess) = self.active_session_mut() {
                        ui.label(
                            RichText::new("Control or spoof the host environment, timestamps, and system data exposed to the neural network for this dialogue.")
                                .color(Color32::from_rgb(148, 163, 184))
                                .size(12.0),
                        );
                        ui.add_space(8.0);

                        // Quick Presets
                        ui.label(RichText::new("QUICK PRIVACY PRESETS:").color(Color32::from_rgb(56, 189, 248)).strong().size(11.5));
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Default Host (Windows 11 / Real Time)").clicked() {
                                sess.context_config = ContextConfig::default();
                                save_needed = true;
                            }
                            if ui.button("🐧 Stealth Linux (Ubuntu / UTC / /home/dev)").clicked() {
                                sess.context_config.enable_system_prompt = true;
                                sess.context_config.enable_temporal_anchor = true;
                                sess.context_config.spoof_datetime = "Monday, Jan 15, 2024, 12:00:00 (UTC)".to_string();
                                sess.context_config.enable_host_env = true;
                                sess.context_config.spoof_os = "linux".to_string();
                                sess.context_config.spoof_arch = "x86_64".to_string();
                                sess.context_config.spoof_cwd = "/home/developer/workspace".to_string();
                                sess.context_config.spoof_user_home = "/home/developer".to_string();
                                sess.context_config.mask_tool_outputs = true;
                                save_needed = true;
                            }
                            if ui.button("🍎 macOS ARM64 (/Users/developer)").clicked() {
                                sess.context_config.enable_system_prompt = true;
                                sess.context_config.enable_temporal_anchor = true;
                                sess.context_config.enable_host_env = true;
                                sess.context_config.spoof_os = "macos".to_string();
                                sess.context_config.spoof_arch = "aarch64".to_string();
                                sess.context_config.spoof_cwd = "/Users/developer/code".to_string();
                                sess.context_config.spoof_user_home = "/Users/developer".to_string();
                                sess.context_config.mask_tool_outputs = true;
                                save_needed = true;
                            }
                            if ui.button("🚫 Zero Host Data (Stealth / Pure Prompt)").clicked() {
                                sess.context_config.enable_system_prompt = true;
                                sess.context_config.enable_temporal_anchor = false;
                                sess.context_config.enable_host_env = false;
                                sess.context_config.enable_tools = false;
                                sess.context_config.mask_tool_outputs = true;
                                save_needed = true;
                            }
                            if ui.button("⛔ Disable System Prompt Entirely").clicked() {
                                sess.context_config.enable_system_prompt = false;
                                save_needed = true;
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                            // 1. Master System Prompt Toggle
                            if ui.checkbox(&mut sess.context_config.enable_system_prompt, RichText::new("Enable System Prompt").strong()).changed() {
                                save_needed = true;
                            }
                            if !sess.context_config.enable_system_prompt {
                                ui.label(
                                    RichText::new("⚠️ System prompt is completely disabled. The model will receive NO host information, date/time, or operating instructions.")
                                        .color(Color32::from_rgb(245, 158, 11))
                                        .italics()
                                        .size(11.5),
                                );
                            }
                            ui.add_space(6.0);

                            // 2. Temporal Anchor
                            Frame::none()
                                .fill(Color32::from_rgb(16, 20, 28))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(28, 36, 50)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("📅 Temporal Anchor (Date & Time)").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                    if ui.checkbox(&mut sess.context_config.enable_temporal_anchor, "Send Date & Time to LLM").changed() {
                                        save_needed = true;
                                    }

                                    if sess.context_config.enable_temporal_anchor {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Spoofed Date/Time:").size(12.0));
                                            if ui.add(
                                                egui::TextEdit::singleline(&mut sess.context_config.spoof_datetime)
                                                    .hint_text("Leave empty for real local time")
                                                    .desired_width(ui.available_width()),
                                            ).changed() {
                                                save_needed = true;
                                            }
                                        });
                                    }
                                });

                            ui.add_space(8.0);

                            // 3. Host Environment Spoofing
                            Frame::none()
                                .fill(Color32::from_rgb(16, 20, 28))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(28, 36, 50)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("💻 Host Environment & Paths").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                    if ui.checkbox(&mut sess.context_config.enable_host_env, "Send Host Environment Block (OS, CWD, Home dir)").changed() {
                                        save_needed = true;
                                    }

                                    if sess.context_config.enable_host_env {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Spoofed OS:").size(12.0));
                                            if ui.add(
                                                egui::TextEdit::singleline(&mut sess.context_config.spoof_os)
                                                    .hint_text("e.g. linux, windows, macos")
                                                    .desired_width(120.0),
                                            ).changed() {
                                                save_needed = true;
                                            }

                                            ui.add_space(8.0);
                                            ui.label(RichText::new("Arch:").size(12.0));
                                            if ui.add(
                                                egui::TextEdit::singleline(&mut sess.context_config.spoof_arch)
                                                    .hint_text("e.g. x86_64, aarch64")
                                                    .desired_width(ui.available_width()),
                                            ).changed() {
                                                save_needed = true;
                                            }
                                        });

                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Spoofed CWD:").size(12.0));
                                            if ui.add(
                                                egui::TextEdit::singleline(&mut sess.context_config.spoof_cwd)
                                                    .hint_text("e.g. /home/user/project")
                                                    .desired_width(ui.available_width()),
                                            ).changed() {
                                                save_needed = true;
                                            }
                                        });

                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("Spoofed Home:").size(12.0));
                                            if ui.add(
                                                egui::TextEdit::singleline(&mut sess.context_config.spoof_user_home)
                                                    .hint_text("e.g. /home/developer")
                                                    .desired_width(ui.available_width()),
                                            ).changed() {
                                                save_needed = true;
                                            }
                                        });
                                    }

                                    ui.add_space(4.0);
                                    if ui.checkbox(&mut sess.context_config.mask_tool_outputs, "Sanitize real user paths & username in tool outputs").changed() {
                                        save_needed = true;
                                    }
                                });

                            ui.add_space(8.0);

                            // 4. Tools Capabilities
                            Frame::none()
                                .fill(Color32::from_rgb(16, 20, 28))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(28, 36, 50)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("⚡ Tools & Execution").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                    if ui.checkbox(&mut sess.context_config.enable_tools, "Enable Tool Calling (Python, Shell, Web Search, Web Scout)").changed() {
                                        save_needed = true;
                                    }
                                });

                            ui.add_space(8.0);

                            // 5. Custom Directives
                            Frame::none()
                                .fill(Color32::from_rgb(16, 20, 28))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(28, 36, 50)))
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("📝 Custom Directives / Persona").color(Color32::from_rgb(56, 189, 248)).strong().size(12.5));
                                    ui.horizontal(|ui| {
                                        ui.label("Mode:");
                                        let mut is_override = sess.context_config.custom_prompt_mode == CustomPromptMode::Override;
                                        if ui.radio_value(&mut is_override, false, "Append to Default Directives").changed() {
                                            sess.context_config.custom_prompt_mode = CustomPromptMode::Append;
                                            save_needed = true;
                                        }
                                        if ui.radio_value(&mut is_override, true, "Override Entirely (Clean Slate)").changed() {
                                            sess.context_config.custom_prompt_mode = CustomPromptMode::Override;
                                            save_needed = true;
                                        }
                                    });

                                    ui.add_space(4.0);
                                    if ui.add(
                                        egui::TextEdit::multiline(&mut sess.context_config.custom_system_prompt)
                                            .hint_text("Enter custom directives, background story, or persona rules...")
                                            .desired_rows(4)
                                            .desired_width(ui.available_width()),
                                    ).changed() {
                                        save_needed = true;
                                    }
                                });

                            ui.add_space(8.0);

                            // 6. Live Inspector
                            let preview_prompt = build_configured_system_prompt(&sess.context_config);
                            egui::CollapsingHeader::new(
                                RichText::new(format!("👁️ Live Inspector: Exact Context Sent to LLM ({} chars)", preview_prompt.len()))
                                    .color(Color32::from_rgb(52, 211, 153))
                                    .size(12.0),
                            )
                            .default_open(false)
                            .show(ui, |ui| {
                                Frame::none()
                                    .fill(Color32::from_rgb(10, 12, 16))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(24, 30, 42)))
                                    .rounding(Rounding::same(6.0))
                                    .inner_margin(Margin::same(8.0))
                                    .show(ui, |ui| {
                                        if preview_prompt.is_empty() {
                                            ui.label(RichText::new("<NO SYSTEM PROMPT SENT>").color(Color32::GRAY).italics());
                                        } else {
                                            ui.label(RichText::new(&preview_prompt).monospace().color(Color32::from_rgb(167, 243, 208)).size(11.0));
                                        }
                                    });
                            });
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Done").strong()).clicked() {
                                self.show_context_settings = false;
                                save_needed = true;
                            }
                        });
                    }
                });

            if !open {
                self.show_context_settings = false;
            }

            if save_needed {
                self.save_active_session();
            }
        }
    }
}
