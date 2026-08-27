//! Client-side presentation adapter for daemon-owned composer completion.
//!
//! Matching, intent steering, project scanning, and path resolution happen in
//! Muta. Mutx only requests results, translates wire offsets into Rust byte
//! offsets, and renders/applies the returned edits.

use crate::App;
use crate::composer::{composer_text_width, composer_wrapped_pos};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDoc {
    pub name: String,
    /// The single prose introduction (contract has exactly one field).
    pub summary: String,
    pub usage: Vec<String>,
    pub examples: Vec<(String, String)>,
    pub intent_keywords: Vec<String>,
    pub category: Option<String>,
    /// First-token verbs (`/schedule list` → `list`) with their own
    /// introductions; rendered after the parent's usage block.
    pub subcommands: Vec<(String, String)>,
}

impl CommandDoc {
    pub fn from_spec(spec: &muta_contracts::CommandSpec) -> Self {
        Self {
            name: spec.name.clone(),
            summary: spec.summary.clone(),
            usage: spec.usage.clone(),
            examples: spec
                .examples
                .iter()
                .map(|example| (example.command.clone(), example.description.clone()))
                .collect(),
            intent_keywords: spec.intent_keywords.clone(),
            category: spec.category.clone(),
            subcommands: spec
                .subcommands
                .iter()
                .map(|sub| (sub.name.clone(), sub.summary.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    #[default]
    None,
    Slash,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompletionItemKind {
    #[default]
    Slash,
    IntentSuggestion {
        matched_intent: String,
        reason: String,
    },
    PathFile,
    PathDir,
    PathExplicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub description: String,
    pub insert_text: String,
    pub replace_start: usize,
    pub replace_end: usize,
    pub kind: CompletionItemKind,
    pub doc: Option<CommandDoc>,
}

impl Completion {
    pub fn whole_input(label: &str, description: &str, input_len: usize) -> Self {
        Self {
            label: label.to_string(),
            description: description.to_string(),
            insert_text: label.to_string(),
            replace_start: 0,
            replace_end: input_len,
            kind: CompletionItemKind::Slash,
            doc: None,
        }
    }

    fn from_backend(input: &str, item: &muta_contracts::InputCompletion) -> Option<Self> {
        let replace_start = char_to_byte(input, item.replace_start)?;
        let replace_end = char_to_byte(input, item.replace_end)?;
        if replace_start > replace_end {
            return None;
        }
        let kind = match item.kind {
            muta_contracts::InputCompletionKind::Slash => CompletionItemKind::Slash,
            muta_contracts::InputCompletionKind::Intent => CompletionItemKind::IntentSuggestion {
                matched_intent: String::new(),
                reason: item.description.clone(),
            },
            muta_contracts::InputCompletionKind::PathFile => CompletionItemKind::PathFile,
            muta_contracts::InputCompletionKind::PathDir => CompletionItemKind::PathDir,
            muta_contracts::InputCompletionKind::PathExplicit => CompletionItemKind::PathExplicit,
        };
        Some(Self {
            label: item.label.clone(),
            description: item.description.clone(),
            insert_text: item.insert_text.clone(),
            replace_start,
            replace_end,
            kind,
            doc: item.command.as_ref().map(CommandDoc::from_spec),
        })
    }
}

fn char_to_byte(input: &str, char_index: usize) -> Option<usize> {
    if char_index == input.chars().count() {
        Some(input.len())
    } else {
        input.char_indices().nth(char_index).map(|(byte, _)| byte)
    }
}

pub fn completion_anchor_x(
    input: &str,
    byte_cursor: usize,
    input_rect: mutx_engine::Rect,
    kind: CompletionKind,
) -> u16 {
    const COMPOSER_PROMPT_PREFIX_COLS: u16 = 2;
    let text_width = composer_text_width(input_rect.width as usize);
    let trigger_byte = match kind {
        CompletionKind::Path => mention_range_at(input, byte_cursor)
            .map(|(start, _)| start)
            .unwrap_or(0),
        _ => 0,
    };
    let (_, col) = composer_wrapped_pos(input, text_width, trigger_byte);
    input_rect.x + COMPOSER_PROMPT_PREFIX_COLS + col.min(text_width) as u16
}

pub fn resolved_slash_command_len(
    input: &str,
    catalog: &muta_contracts::CommandCatalog,
) -> Option<usize> {
    if !input.starts_with('/') {
        return None;
    }
    let token = input
        .trim()
        .split_once(char::is_whitespace)
        .map(|(name, _)| name)
        .unwrap_or_else(|| input.trim());
    (token.len() > 1 && catalog.recognizes(token)).then_some(token.len())
}

pub(super) fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    if cursor_byte > input.len() || !input.is_char_boundary(cursor_byte) {
        return None;
    }
    let mut chars_before = input[..cursor_byte].char_indices().collect::<Vec<_>>();
    while let Some((idx, character)) = chars_before.pop() {
        if character.is_whitespace() {
            return None;
        }
        if character == '@' {
            let preceded_by_space = chars_before
                .last()
                .map(|(_, previous)| previous.is_whitespace())
                .unwrap_or(true);
            return preceded_by_space.then_some((idx, cursor_byte));
        }
    }
    None
}

impl App {
    pub fn completion_kind(&self) -> CompletionKind {
        if self.input.starts_with('/') {
            CompletionKind::Slash
        } else if self.active_mention_range().is_some() {
            CompletionKind::Path
        } else {
            CompletionKind::None
        }
    }

    pub fn completion_trigger_text_present(&self) -> bool {
        match self.completion_kind() {
            CompletionKind::None => false,
            CompletionKind::Slash => {
                !self.input.trim().is_empty() && !self.known_exact_slash_input()
            }
            CompletionKind::Path => true,
        }
    }

    fn known_exact_slash_input(&self) -> bool {
        let Some(first) = self.input.split_whitespace().next() else {
            return false;
        };
        self.input.starts_with('/')
            && self.input.trim() == first
            && self.command_catalog.recognizes(first)
    }

    pub fn anchor_completion_selection(&mut self, completions: &[Completion]) {
        let input_len = self.input.len();
        let exact = completions.iter().any(|item| {
            item.replace_start == 0 && item.replace_end == input_len && item.label == self.input
        });
        let visible = !completions.is_empty() && !exact;
        match (visible, self.suggestion_index) {
            (false, _) => self.suggestion_index = None,
            (true, None) => self.suggestion_index = Some(0),
            (true, Some(index)) => {
                self.suggestion_index = Some(index.min(completions.len() - 1));
            }
        }
    }

    /// Current daemon response translated into renderer-native byte edits.
    pub fn completions(&mut self) -> Vec<Completion> {
        let cursor = self.cursor_position;
        let items = if self.completion_response_input.as_deref() == Some(self.input.as_str())
            && self.completion_response_cursor == cursor
        {
            self.backend_completions.clone()
        } else {
            #[cfg(test)]
            {
                muta_runtime::input_completion::complete_for_frontend_test(
                    self.command_catalog.clone(),
                    self.cwd.clone(),
                    &self.input,
                    cursor,
                )
            }
            #[cfg(not(test))]
            {
                Vec::new()
            }
        };
        items
            .iter()
            .filter_map(|item| Completion::from_backend(&self.input, item))
            .collect()
    }

    /// Send a completion request when the composer state changed.
    pub fn refresh_backend_completion_request(&mut self) {
        let cursor = self.cursor_position;
        let state = (self.input.clone(), cursor);
        if self.completion_requested.as_ref() == Some(&state) {
            return;
        }
        self.completion_requested = Some(state.clone());
        self.backend_completions.clear();
        self.completion_response_input = None;
        self.completion_response_cursor = 0;
        self.completion_request_id = self.completion_request_id.wrapping_add(1);
        let _ = self
            .tx
            .send(muta_contracts::AgentRequest::CompleteComposer {
                request_id: self.completion_request_id,
                text: state.0,
                cursor,
            });
    }

    pub fn apply_backend_completions(
        &mut self,
        request_id: u64,
        input: String,
        cursor: usize,
        items: Vec<muta_contracts::InputCompletion>,
    ) {
        if request_id != self.completion_request_id
            || input != self.input
            || cursor != self.cursor_position
        {
            return;
        }
        self.completion_response_input = Some(input);
        self.completion_response_cursor = cursor;
        self.backend_completions = items;
    }

    pub fn active_mention_range(&self) -> Option<(usize, usize)> {
        mention_range_at(&self.input, self.byte_cursor())
    }
}
