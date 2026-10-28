use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub output: Option<String>,
    pub status: ToolStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ToolStatus {
    Pending,
    Running,
    Success,
    Failed,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatImageAttachment {
    pub id: String,
    pub mime_type: String,
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
}

impl ChatImageAttachment {
    pub fn raw_bytes(&self) -> Option<Vec<u8>> {
        if let Some(pos) = self.data_url.find(',') {
            let b64 = &self.data_url[pos + 1..];
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(b64).ok()
        } else {
            None
        }
    }

    pub fn from_image_bytes(bytes: &[u8]) -> Result<Self, String> {
        let dynamic_img =
            image::load_from_memory(bytes).map_err(|e| format!("Failed to decode image: {}", e))?;

        let (dynamic_img, (w, h)) = if dynamic_img.width() > 1536 || dynamic_img.height() > 1536 {
            let resized = dynamic_img.resize(1536, 1536, image::imageops::FilterType::Lanczos3);
            let dims = (resized.width(), resized.height());
            (resized, dims)
        } else {
            let dims = (dynamic_img.width(), dynamic_img.height());
            (dynamic_img, dims)
        };

        let mut cursor = std::io::Cursor::new(Vec::new());
        dynamic_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode PNG: {}", e))?;
        let png_bytes = cursor.into_inner();
        let byte_size = png_bytes.len();

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let data_url = format!("data:image/png;base64,{}", b64);

        Ok(Self {
            id: Uuid::new_v4().to_string(),
            mime_type: "image/png".to_string(),
            data_url,
            width: w,
            height: h,
            byte_size,
        })
    }

    pub fn from_file_path(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;
        Self::from_image_bytes(&bytes)
    }

    pub fn from_clipboard() -> Result<Self, String> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("Clipboard error: {}", e))?;

        // 1. Try native clipboard image
        if let Ok(img_data) = clipboard.get_image() {
            let rgba_img = image::RgbaImage::from_raw(
                img_data.width as u32,
                img_data.height as u32,
                img_data.bytes.into_owned(),
            )
            .ok_or_else(|| "Failed to construct RGBA image from clipboard buffer".to_string())?;

            let dynamic_img = image::DynamicImage::ImageRgba8(rgba_img);
            let (dynamic_img, (w, h)) = if dynamic_img.width() > 1536 || dynamic_img.height() > 1536
            {
                let resized = dynamic_img.resize(1536, 1536, image::imageops::FilterType::Lanczos3);
                let dims = (resized.width(), resized.height());
                (resized, dims)
            } else {
                let dims = (dynamic_img.width(), dynamic_img.height());
                (dynamic_img, dims)
            };

            let mut cursor = std::io::Cursor::new(Vec::new());
            dynamic_img
                .write_to(&mut cursor, image::ImageFormat::Png)
                .map_err(|e| format!("Failed to encode clipboard PNG: {}", e))?;
            let png_bytes = cursor.into_inner();
            let byte_size = png_bytes.len();

            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            let data_url = format!("data:image/png;base64,{}", b64);

            return Ok(Self {
                id: Uuid::new_v4().to_string(),
                mime_type: "image/png".to_string(),
                data_url,
                width: w,
                height: h,
                byte_size,
            });
        }

        // 2. Try file path in clipboard text
        if let Ok(text) = clipboard.get_text() {
            let trimmed = text.trim().trim_matches('"').trim();
            let p = Path::new(trimmed);
            if p.is_file() {
                if let Some(ext) = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_lowercase())
                {
                    if matches!(
                        ext.as_str(),
                        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
                    ) {
                        return Self::from_file_path(p);
                    }
                }
            }
        }

        Err("No image or image file path found in clipboard".to_string())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub images: Vec<ChatImageAttachment>,
    pub timestamp: DateTime<Utc>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: MessageRole::User,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
            timestamp: Utc::now(),
        }
    }

    pub fn user_with_images(content: impl Into<String>, images: Vec<ChatImageAttachment>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: MessageRole::User,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            images,
            timestamp: Utc::now(),
        }
    }

    pub fn assistant(content: impl Into<String>, thinking: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: MessageRole::Assistant,
            content: content.into(),
            thinking,
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
            timestamp: Utc::now(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: MessageRole::System,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
            timestamp: Utc::now(),
        }
    }

    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: MessageRole::Tool,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            images: Vec::new(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[derive(Default)]
pub enum CustomPromptMode {
    #[default]
    Append,
    Override,
}


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContextConfig {
    #[serde(default = "default_true")]
    pub enable_system_prompt: bool,

    #[serde(default = "default_true")]
    pub enable_temporal_anchor: bool,
    #[serde(default)]
    pub spoof_datetime: String,

    #[serde(default = "default_true")]
    pub enable_host_env: bool,
    #[serde(default)]
    pub spoof_os: String,
    #[serde(default)]
    pub spoof_arch: String,
    #[serde(default)]
    pub spoof_cwd: String,
    #[serde(default)]
    pub spoof_user_home: String,

    #[serde(default = "default_true")]
    pub enable_tools: bool,

    #[serde(default = "default_true")]
    pub mask_tool_outputs: bool,

    #[serde(default)]
    pub custom_system_prompt: String,
    #[serde(default)]
    pub custom_prompt_mode: CustomPromptMode,
}

fn default_true() -> bool {
    true
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enable_system_prompt: true,
            enable_temporal_anchor: true,
            spoof_datetime: String::new(),
            enable_host_env: true,
            spoof_os: String::new(),
            spoof_arch: String::new(),
            spoof_cwd: String::new(),
            spoof_user_home: String::new(),
            enable_tools: true,
            mask_tool_outputs: true,
            custom_system_prompt: String::new(),
            custom_prompt_mode: CustomPromptMode::Append,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<ChatMessage>,
    pub active_model: String,
    #[serde(default)]
    pub context_config: ContextConfig,
    #[serde(default)]
    pub is_deep_research: bool,
}

impl Session {
    pub fn new(model: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            title: "New Chat".to_string(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            active_model: model.into(),
            context_config: ContextConfig::default(),
            is_deep_research: false,
        }
    }

    pub fn update_title_from_first_message(&mut self) {
        if self.title == "New Chat" {
            if let Some(first_user) = self.messages.iter().find(|m| m.role == MessageRole::User) {
                let full_text = first_user.content.trim();
                // Strip known command directives if present (e.g. DEEP RESEARCH DIRECTIVE, CALC DIRECTIVE)
                let cleaned: String = if let Some(idx) = full_text.find("\n\n---\n\n") {
                    full_text[idx + 7..].trim().to_string()
                } else if full_text.starts_with("DEEP RESEARCH DIRECTIVE:") {
                    let lines: Vec<&str> = full_text
                        .lines()
                        .skip_while(|line| {
                            let t = line.trim();
                            t.starts_with("DEEP RESEARCH DIRECTIVE:")
                                || t.starts_with("You are")
                                || t.starts_with("1.")
                                || t.starts_with("2.")
                                || t.starts_with("3.")
                                || t.starts_with("4.")
                                || t.starts_with("5.")
                                || t.starts_with("6.")
                                || t.is_empty()
                        })
                        .collect();
                    lines.join(" ").trim().to_string()
                } else {
                    full_text.to_string()
                };

                let effective_text = if cleaned.is_empty() {
                    full_text
                } else {
                    cleaned.as_str()
                };

                let clean = effective_text
                    .lines()
                    .find(|line: &&str| !line.trim().is_empty())
                    .unwrap_or(effective_text)
                    .trim();
                let truncated: String = clean.chars().take(36).collect();
                if !truncated.is_empty() {
                    self.title = truncated;
                }
            }
        }
    }
}

use crate::vault::VaultKey;

pub struct SessionManager {
    pub storage_dir: PathBuf,
    pub vault_key: Option<VaultKey>,
}

impl SessionManager {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        let storage_dir = base_dir.as_ref().join("sessions");
        let _ = fs::create_dir_all(&storage_dir);
        Self {
            storage_dir,
            vault_key: None,
        }
    }

    pub fn set_vault_key(&mut self, key: VaultKey) {
        self.vault_key = Some(key);
    }

    pub fn save_session(&self, session: &Session) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(key) = &self.vault_key {
            // High-performance encrypted binary storage via postcard + AEAD
            let postcard_bytes = postcard::to_allocvec(session)?;
            let encrypted = key.encrypt(&postcard_bytes)?;
            let silo_path = self.storage_dir.join(format!("{}.silo", session.id));
            fs::write(silo_path, encrypted)?;

            // Clean up legacy plaintext JSON if it exists
            let legacy_json = self.storage_dir.join(format!("{}.json", session.id));
            if legacy_json.exists() {
                let _ = fs::remove_file(legacy_json);
            }
        } else {
            // Fallback plain JSON
            let file_path = self.storage_dir.join(format!("{}.json", session.id));
            let json = serde_json::to_string_pretty(session)?;
            fs::write(file_path, json)?;
        }
        Ok(())
    }

    pub fn load_all_sessions(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.storage_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path.extension().and_then(|s| s.to_str());

                if ext == Some("silo") {
                    if let Some(key) = &self.vault_key {
                        if let Ok(bytes) = fs::read(&path) {
                            if let Ok(decrypted) = key.decrypt(&bytes) {
                                if let Ok(session) = postcard::from_bytes::<Session>(&decrypted) {
                                    sessions.push(session);
                                }
                            }
                        }
                    }
                } else if ext == Some("json") {
                    // Backward compatibility: load legacy plaintext JSON and migrate to .silo
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Ok(session) = serde_json::from_str::<Session>(&content) {
                            if self.vault_key.is_some() {
                                let _ = self.save_session(&session);
                            }
                            sessions.push(session);
                        }
                    }
                }
            }
        }
        // Sort by updated_at descending (most recent first)
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions
    }

    pub fn delete_session(&self, id: &str) -> bool {
        let silo_path = self.storage_dir.join(format!("{}.silo", id));
        let json_path = self.storage_dir.join(format!("{}.json", id));
        let r1 = fs::remove_file(silo_path).is_ok();
        let r2 = fs::remove_file(json_path).is_ok();
        r1 || r2
    }
}
