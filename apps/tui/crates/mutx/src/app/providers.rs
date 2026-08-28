//! Provider modals state: preset chooser, custom editor fields, the OAuth flow, model suggestions, delete staging.

use super::*;

impl App {
    /// Open the curated preset chooser — the "Add preset connection" entry
    /// point.
    /// The chat draft is already parked in `stashed_input` (the Connections list
    /// stashed it on open); the chooser is a pure list, so the composer line
    /// stays clear.
    pub fn open_preset_chooser(&mut self) {
        if self.active_panel() == Some(crate::surfaces::PanelId::Connections) {
            self.push_transient_surface(Modal::ProviderPreset);
        } else {
            self.replace_transient_surface(Modal::ProviderPreset);
        }
        self.preset_choice = 0;
        self.preset_scroll = 0;
        self.input.clear();
        self.set_cursor(0);
    }

    /// Open the standalone custom-connection editor directly from
    /// Connections. Unlike a preset selection, this is a sibling branch of
    /// the add flow, so the Connections panel stays below it on the transient
    /// navigation stack.
    pub fn open_custom_connection_editor(&mut self) {
        self.seed_custom_provider_from_preset(&crate::providers::CUSTOM_CONNECTION);
        if self.active_panel() == Some(crate::surfaces::PanelId::Connections) {
            self.push_transient_surface(Modal::CustomProvider);
        } else {
            self.replace_transient_surface(Modal::CustomProvider);
        }
        self.input.clear();
        self.set_cursor(0);
    }

    /// Move the preset-chooser selection, wrapping at the ends.
    pub fn move_preset_choice(&mut self, forward: bool) {
        let n = crate::PROVIDER_PRESETS.len();
        if n == 0 {
            return;
        }
        self.preset_choice = if forward {
            (self.preset_choice + 1) % n
        } else {
            (self.preset_choice + n - 1) % n
        };
    }

    /// Seed create-mode buffers from `preset` without opening the editor.
    pub fn seed_custom_provider_from_preset(&mut self, preset: &ProviderPreset) {
        self.custom_edit_id = None;
        self.custom_fields = preset.fields();
        self.custom_field = 0;
        self.custom_protocol_wire = preset.protocol.to_string();
        self.custom_models = preset.models.iter().map(|m| m.to_string()).collect();
        self.custom_url_hint = preset.url_hint.to_string();
        self.custom_user_agent = preset.user_agent.map(str::to_string);
        self.custom_auth = preset.auth;
        self.custom_preset_id = Some(preset.id.to_string());
        self.custom_suggest_index = 0;
        self.custom_name.clear();
        self.custom_base_url = preset.default_url.map(str::to_string).unwrap_or_default();
        self.custom_token.clear();
        self.custom_model = self
            .custom_model_candidates()
            .first()
            .map(|m| m.to_string())
            .unwrap_or_default();
    }

    /// Open the provider editor seeded from `preset` (create mode) on the Name
    /// field. The composer line is borrowed for the focused Name field.
    pub fn open_custom_provider_editor(&mut self, preset: &ProviderPreset) {
        self.seed_custom_provider_from_preset(preset);
        self.replace_transient_surface(Modal::CustomProvider);
        self.input.clear();
        self.set_cursor(0);
    }

    /// Open the OAuth waiting sheet and seed create buffers from `preset`.
    pub fn begin_oauth_add(
        &mut self,
        preset: &ProviderPreset,
        method: muta_contracts::LoginMethod,
    ) {
        self.seed_custom_provider_from_preset(preset);
        self.awaiting_oauth_add = true;
        // The default message mirrors the selected login method: the device
        // flow prints a URL + user code,
        // while the browser flow opens a loopback callback. The auth runner
        // overwrites this with the live URL/code as soon as the device-code
        // request returns.
        self.oauth_pending_message = match method {
            muta_contracts::LoginMethod::Device => "Requesting device code…".to_string(),
            muta_contracts::LoginMethod::Browser => {
                "Complete authorization in your browser (or open the link below).".to_string()
            }
        };
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.replace_transient_surface(Modal::OauthPending);
        self.input.clear();
        self.set_cursor(0);
    }

    /// After OAuth succeeds: name-only editor (default name derived from the
    /// in-flight auth — "xAI" for SuperGrok, "ChatGPT subscription" for the
    /// ChatGPT plan).
    pub fn open_oauth_instance_name_editor(&mut self) {
        self.awaiting_oauth_add = false;
        self.oauth_pending_url.clear();
        self.oauth_pending_user_code.clear();
        self.oauth_pending_message.clear();
        self.oauth_pending_error = None;
        self.oauth_scroll = 0;
        self.replace_transient_surface(Modal::CustomProvider);
        self.custom_fields = vec![CustomField::Name];
        self.custom_field = 0;
        self.custom_edit_id = None;
        let default_name = match self.custom_auth {
            muta_contracts::ChannelAuth::ChatGptOAuth => "ChatGPT subscription",
            muta_contracts::ChannelAuth::CopilotOAuth => "Copilot",
            muta_contracts::ChannelAuth::AntigravityOAuth => "Google Antigravity",
            _ => "xAI",
        };
        self.custom_name = default_name.to_string();
        self.input = default_name.to_string();
        self.set_cursor_end();
    }

    /// Return which OAuth target is currently selected.
    pub fn oauth_selected_target(&self) -> crate::input::OauthCopyTarget {
        if self.oauth_selected_item == 1 && !self.oauth_pending_user_code.is_empty() {
            crate::input::OauthCopyTarget::UserCode
        } else {
            crate::input::OauthCopyTarget::Url
        }
    }

    /// Cycle selection between URL (0) and Code (1) in OAuth Pending sheet.
    pub fn cycle_oauth_selection(&mut self) {
        if !self.oauth_pending_user_code.is_empty() {
            self.oauth_selected_item = if self.oauth_selected_item == 0 { 1 } else { 0 };
        } else {
            self.oauth_selected_item = 0;
        }
    }

    /// Auth mode of a provider picker row (for OAuth re-connect routing).
    pub fn provider_row_auth(&self, id: &str) -> muta_contracts::ChannelAuth {
        self.provider_picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.auth)
            .unwrap_or_default()
    }

    /// Open the provider editor in **edit** mode for an existing user provider,
    /// pre-filling its metadata. The visible fields depend on whether it is a
    /// preset vs custom provider, and its auth type: a custom API-key channel shows
    /// Name / Base URL / Token, a preset API-key channel shows Name / Token, and an
    /// OAuth channel (ChatGPT/Codex, xAI, Copilot, Antigravity) shows Name only.
    /// The Model field is always hidden (models are managed in the Models picker).
    pub fn open_edit_provider_editor(
        &mut self,
        id: String,
        name: String,
        protocol: String,
        base_url: String,
        auth: ChannelAuth,
        is_preset: bool,
    ) {
        if self.active_panel() == Some(crate::surfaces::PanelId::Connections) {
            self.push_transient_surface(Modal::CustomProvider);
        } else {
            self.replace_transient_surface(Modal::CustomProvider);
        }
        self.custom_edit_id = Some(id);
        self.custom_fields = edit_fields(is_preset, auth);
        self.custom_field = 0;
        self.custom_protocol_wire = protocol;
        self.custom_models.clear();
        self.custom_url_hint.clear();
        self.custom_user_agent = None;
        self.custom_auth = auth;
        // Edit mode never carries a preset id: edits to an existing connection
        // are sent as `EditProvider`, which ignores the preset id anyway, and
        // a stray id here must not leak into a later create flow.
        self.custom_preset_id = None;
        self.custom_suggest_index = 0;
        self.custom_name = name.clone();
        self.custom_base_url = base_url;
        self.custom_token.clear();
        self.custom_model.clear();
        self.input = name;
        self.set_cursor_end();
    }

    /// Whether the provider editor is editing an existing provider.
    pub fn custom_is_editing(&self) -> bool {
        self.custom_edit_id.is_some()
    }

    /// The currently focused editor field, or `None` when no editor is open.
    pub fn current_custom_field(&self) -> Option<CustomField> {
        self.custom_fields.get(self.custom_field as usize).copied()
    }

    /// Number of visible fields the editor exposes for the active preset.
    fn custom_field_count(&self) -> u8 {
        self.custom_fields.len().max(1) as u8
    }

    /// The registry model ids matching the editor's protocol wire format — the
    /// Model filter field's candidate pool.
    pub fn custom_model_candidates(&self) -> Vec<&'static str> {
        crate::protocol_model_candidates(&self.custom_protocol_wire)
    }

    /// The model suggestions matching the live filter (`self.input` while the
    /// Model field is focused): protocol candidates that fuzzy-match, plus the
    /// raw typed text as a custom id when it is not already a candidate.
    pub fn custom_model_suggestions(&self) -> Vec<String> {
        let q = self.input.trim();
        let q_clean = muta_contracts::sanitize_model_id(q);
        let mut out: Vec<String> = self
            .custom_model_candidates()
            .into_iter()
            .filter(|id| {
                q.is_empty()
                    || id.contains(q)
                    || (!q_clean.is_empty() && id.contains(&q_clean))
                    || crate::fuzzy::fuzzy_match(id, q).is_some()
                    || (!q_clean.is_empty() && crate::fuzzy::fuzzy_match(id, &q_clean).is_some())
            })
            .map(|s| s.to_string())
            .collect();
        let custom_id = if !q_clean.is_empty() {
            q_clean
        } else {
            q.to_string()
        };
        if !custom_id.is_empty() && !out.iter().any(|m| m == &custom_id) {
            out.push(custom_id.clone());
        }
        // Stable sort: an exact match floats to the top so typing a known id
        // selects it rather than a longer id that merely contains it as a
        // substring (e.g. "gpt-4o" beats "gpt-4o-mini").
        if !custom_id.is_empty() {
            out.sort_by_key(|m| m != &custom_id && m != q);
        }
        out
    }

    /// Commit the highlighted Model suggestion into `custom_model`. No-op off the
    /// Model field (the only filter field).
    fn commit_custom_suggestion(&mut self) {
        if self.current_custom_field() == Some(CustomField::Model) {
            let suggestions = self.custom_model_suggestions();
            if let Some(value) = suggestions.get(self.custom_suggest_index) {
                self.custom_model = value.clone();
            }
        }
    }

    /// Move the Model suggestion highlight, committing the newly-highlighted
    /// suggestion live. When the Model field is NOT focused, scrolls the modal
    /// body instead.
    pub fn move_custom_suggestion(&mut self, forward: bool) {
        if self.current_custom_field() == Some(CustomField::Model) {
            let len = self.custom_model_suggestions().len();
            if len == 0 {
                return;
            }
            self.custom_suggest_index = if forward {
                (self.custom_suggest_index + 1) % len
            } else {
                (self.custom_suggest_index + len - 1) % len
            };
            self.commit_custom_suggestion();
        } else {
            // Non-Model fields: ↑/↓ scroll the modal body.
            if forward {
                self.custom_scroll = self.custom_scroll.saturating_add(1);
            } else {
                self.custom_scroll = self.custom_scroll.saturating_sub(1);
            }
        }
    }

    /// React to a change in the Model filter query: reset the highlight to the
    /// best (first) match and commit it.
    pub fn on_custom_filter_changed(&mut self) {
        if self.current_custom_field() == Some(CustomField::Model) {
            self.custom_suggest_index = 0;
            self.commit_custom_suggestion();
        }
    }

    /// Save the composer line into the focused text field's buffer (Name / Base
    /// URL / Token). The Model field is a filter whose value is already committed
    /// live, so its transient query is discarded.
    pub fn stash_custom_field(&mut self) {
        let value = std::mem::take(&mut self.input);
        match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name = value,
            Some(CustomField::BaseUrl) => self.custom_base_url = value,
            Some(CustomField::Token) => self.custom_token = value,
            _ => {} // Model filter field: value already committed live.
        }
    }

    /// Load the focused field into the composer line: the buffer for a text
    /// field, or a fresh (empty) filter for the Model field, with the suggestion
    /// highlight positioned on the current committed value.
    pub fn load_custom_field(&mut self) {
        self.input = match self.current_custom_field() {
            Some(CustomField::Name) => self.custom_name.clone(),
            Some(CustomField::BaseUrl) => self.custom_base_url.clone(),
            Some(CustomField::Token) => self.custom_token.clone(),
            _ => String::new(),
        };
        self.set_cursor_end();
        if self.current_custom_field() == Some(CustomField::Model) {
            self.custom_suggest_index = self
                .custom_model_suggestions()
                .iter()
                .position(|v| v == &self.custom_model)
                .unwrap_or(0);
        }
    }

    /// Move the provider editor focus (`Tab` / `BackTab`), wrapping across the
    /// active preset's visible fields.
    pub fn cycle_custom_field(&mut self, forward: bool) {
        self.stash_custom_field();
        let n = self.custom_field_count();
        self.custom_field = if forward {
            (self.custom_field + 1) % n
        } else {
            (self.custom_field + n - 1) % n
        };
        self.load_custom_field();
    }

    /// Compute the **Connections** provider rows. Delegates to
    /// `providers_filtered_from` so the input handler and the renderer share
    /// one filter+sort implementation.
    pub fn providers_filtered(&self) -> Vec<RankedProvider> {
        providers_filtered_from(&self.provider_picker, self.picker_query())
    }

    /// Compute the **flat Models** rows — every (provider, model) pair in the
    /// snapshot, filtered by the current picker query. Delegates to
    /// `models_flat_filtered_from` so the input handler and the renderer
    /// share one filter+sort implementation.
    pub fn models_flat_filtered(&self) -> Vec<RankedModel> {
        models_flat_filtered_from(
            &self.provider_picker,
            &self.current_provider,
            &self.current_model,
            self.picker_query(),
        )
    }

    /// Whether the provider with this snapshot id is user-defined (not a
    /// built-in preset). Drives the Connections `e`/`Shift+D` routing and the
    /// Models `d` (remove-model) gate.
    pub fn provider_is_custom(&self, id: &str) -> bool {
        self.provider_picker
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| !row.builtin)
            .unwrap_or(false)
    }

    /// Number of selectable rows in the active picker. Connections counts only
    /// the provider rows (adding a connection is a footer shortcut now, not a
    /// synthetic list row); Models counts the flat (provider, model) rows. Used
    /// to clamp the ↑/↓ selection cursor. Returns 0 when no picker is open.
    pub fn picker_row_count(&self) -> usize {
        match self.active_modal() {
            Modal::Connections => self.providers_filtered().len(),
            Modal::Models => self.models_flat_filtered().len(),
            _ => 0,
        }
    }

    /// Stage the highlighted custom provider for deletion: open the confirm
    /// overlay ([`App::pending_provider_delete`]) over the Connections list
    /// without destroying anything yet. No-op for built-in providers or when an
    /// overlay is already open (prevents re-staging). Driven by the `Shift+D`
    /// → `DeleteProvider` arm.
    pub fn stage_provider_delete(&mut self) {
        if self.active_modal() != Modal::Connections || self.pending_provider_delete.is_some() {
            return;
        }
        let ranked = self.providers_filtered();
        if let Some(row) = ranked.get(self.modal_index).or_else(|| ranked.first())
            && !row.builtin
        {
            self.pending_provider_delete = Some(row.id.clone());
            self.provider_delete_focus = ProviderDeleteChoice::default();
        }
    }

    /// Confirm the staged deletion: dispatch `AgentRequest::DeleteProvider` for
    /// the staged id and tear the overlay down. Returns `Some(request)` when a
    /// deletion was staged (the harness applies it), `None` when the overlay
    /// was not open. Driven by the overlay's Enter-on-Delete. Decrementing
    /// `modal_index` mirrors the picker's other removal paths so the cursor
    /// lands on a valid row once this row vanishes.
    pub fn confirm_provider_delete(&mut self) -> Option<AgentRequest> {
        let id = self.pending_provider_delete.take()?;
        self.modal_index = self.modal_index.saturating_sub(1);
        self.provider_delete_focus = ProviderDeleteChoice::default();
        Some(AgentRequest::DeleteProvider { id })
    }

    /// Cancel the staged deletion: drop the staged id and return keyboard
    /// focus to the Connections list. The modal itself stays open.
    /// Driven by the overlay's Esc / Ctrl+C / Enter-on-Cancel.
    pub fn cancel_provider_delete(&mut self) {
        self.pending_provider_delete = None;
        self.provider_delete_focus = ProviderDeleteChoice::default();
    }
}
