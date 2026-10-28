use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum ParserState {
    Content,
    Thinking,
    ToolDrafting { tag_name: String },
}

pub struct StreamParser {
    pub state: ParserState,
    buffer: String,
    pub accumulated_thinking: String,
    pub accumulated_content: String,
    pub accumulated_tool_draft: String,
}

#[derive(Debug, Clone)]
pub enum ParsedChunk {
    Content(String),
    Thinking(String),
    ToolDraft(String),
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            state: ParserState::Content,
            buffer: String::new(),
            accumulated_thinking: String::new(),
            accumulated_content: String::new(),
            accumulated_tool_draft: String::new(),
        }
    }

    pub fn push_chunk(&mut self, text: &str) -> Vec<ParsedChunk> {
        self.buffer.push_str(text);
        let mut outputs = Vec::new();

        while !self.buffer.is_empty() {
            match &self.state {
                ParserState::Content => {
                    // Check for stray </think>
                    if self.buffer.starts_with("</think>") {
                        self.buffer = self.buffer["</think>".len()..].trim_start().to_string();
                        continue;
                    }

                    let think_pos = self.buffer.find("<think>");
                    let tool_tags = ["<tool_call>", "<invoke", "<function=", "<function_call>"];
                    let mut tool_starts = Vec::new();
                    for t in &tool_tags {
                        if let Some(pos) = self.buffer.find(t) {
                            tool_starts.push((pos, *t));
                        }
                    }
                    tool_starts.sort_by_key(|k| k.0);

                    // Avoid slicing incomplete tags like "<th", "<inv"
                    if let Some(last_lt) = self.buffer.rfind('<') {
                        if !self.buffer[last_lt..].contains('>')
                            && (think_pos.is_none() && tool_starts.is_empty())
                        {
                            let clean = &self.buffer[..last_lt];
                            if !clean.is_empty() {
                                let stripped = clean_tags(clean);
                                if !stripped.is_empty() {
                                    self.accumulated_content.push_str(&stripped);
                                    outputs.push(ParsedChunk::Content(stripped));
                                }
                            }
                            self.buffer = self.buffer[last_lt..].to_string();
                            break;
                        }
                    }

                    if let Some(tpos) = think_pos {
                        if tool_starts.is_empty() || tpos < tool_starts[0].0 {
                            let pre = &self.buffer[..tpos];
                            let stripped = clean_tags(pre);
                            if !stripped.is_empty() {
                                self.accumulated_content.push_str(&stripped);
                                outputs.push(ParsedChunk::Content(stripped));
                            }
                            self.state = ParserState::Thinking;
                            self.buffer = self.buffer[tpos + "<think>".len()..].to_string();
                            continue;
                        }
                    }

                    if !tool_starts.is_empty() {
                        let (tool_pos, tag) = tool_starts[0];
                        let pre = &self.buffer[..tool_pos];
                        let stripped = clean_tags(pre);
                        if !stripped.is_empty() {
                            self.accumulated_content.push_str(&stripped);
                            outputs.push(ParsedChunk::Content(stripped));
                        }

                        // Find end of the opening tag '>'
                        if let Some(gt_pos) = self.buffer[tool_pos..].find('>') {
                            let end_of_tag = tool_pos + gt_pos + 1;
                            self.state = ParserState::ToolDrafting {
                                tag_name: tag.to_string(),
                            };
                            self.buffer = self.buffer[end_of_tag..].to_string();
                            continue;
                        } else if tag == "<function=" {
                            if let Some(nl) = self.buffer[tool_pos..].find('\n') {
                                let end_of_tag = tool_pos + nl + 1;
                                self.state = ParserState::ToolDrafting {
                                    tag_name: tag.to_string(),
                                };
                                self.buffer = self.buffer[end_of_tag..].to_string();
                                continue;
                            } else {
                                break;
                            }
                        } else {
                            // Incomplete opening tag
                            break;
                        }
                    }

                    // Pure content chunk
                    let to_print = self.buffer.clone();
                    let stripped = clean_tags(&to_print);
                    if !stripped.is_empty() {
                        self.accumulated_content.push_str(&stripped);
                        outputs.push(ParsedChunk::Content(stripped));
                    }
                    self.buffer.clear();
                    break;
                }

                ParserState::Thinking => {
                    if let Some(end_think) = self.buffer.find("</think>") {
                        let thinking_chunk = &self.buffer[..end_think];
                        if !thinking_chunk.is_empty() {
                            self.accumulated_thinking.push_str(thinking_chunk);
                            outputs.push(ParsedChunk::Thinking(thinking_chunk.to_string()));
                        }
                        self.state = ParserState::Content;
                        let after_think = &self.buffer[end_think + "</think>".len()..];
                        self.buffer = after_think
                            .trim_start_matches(['\n', '\r'])
                            .to_string();
                        continue;
                    } else {
                        // Avoid partial </think
                        if let Some(last_lt) = self.buffer.rfind('<') {
                            if !self.buffer[last_lt..].contains('>') {
                                let safe = &self.buffer[..last_lt];
                                if !safe.is_empty() {
                                    self.accumulated_thinking.push_str(safe);
                                    outputs.push(ParsedChunk::Thinking(safe.to_string()));
                                }
                                self.buffer = self.buffer[last_lt..].to_string();
                                break;
                            }
                        }

                        let safe = self.buffer.clone();
                        self.accumulated_thinking.push_str(&safe);
                        outputs.push(ParsedChunk::Thinking(safe));
                        self.buffer.clear();
                        break;
                    }
                }

                ParserState::ToolDrafting { tag_name: _ } => {
                    let close_tags = [
                        "</tool_call>",
                        "</invoke>",
                        "</function>",
                        "</function_call>",
                    ];
                    let mut found_close: Option<(usize, &'static str)> = None;

                    for ct in &close_tags {
                        if let Some(pos) = self.buffer.find(ct) {
                            if found_close.is_none() || pos < found_close.unwrap().0 {
                                found_close = Some((pos, *ct));
                            }
                        }
                    }

                    if let Some((close_pos, close_tag)) = found_close {
                        let draft_piece = &self.buffer[..close_pos];
                        self.accumulated_tool_draft.push_str(draft_piece);
                        outputs.push(ParsedChunk::ToolDraft(draft_piece.to_string()));

                        self.state = ParserState::Content;
                        self.buffer = self.buffer[close_pos + close_tag.len()..].to_string();
                        continue;
                    } else {
                        let safe = self.buffer.clone();
                        self.accumulated_tool_draft.push_str(&safe);
                        outputs.push(ParsedChunk::ToolDraft(safe));
                        self.buffer.clear();
                        break;
                    }
                }
            }
        }

        outputs
    }
}

fn clean_tags(text: &str) -> String {
    if let Ok(re) = Regex::new(
        r"</?(?:tool_call|invoke|function|function_call|parameter|arguments|think)[^>]*>",
    ) {
        re.replace_all(text, "").to_string()
    } else {
        text.to_string()
    }
}
