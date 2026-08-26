//! Provider/route settings tests: template editors, capability overrides, custom provider submission, model selection.

use super::*;


#[test]
fn provider_retry_state_formats_summary_and_timing() {
    let now = std::time::Instant::now();
    let state = ProviderRetryState {
        attempt: 2,
        max_attempts: 16,
        retry_at: now + std::time::Duration::from_millis(6_600),
        failure: "HTTP 429: rate limited".to_string(),
    };
    let summary = state.summary(now);
    assert_eq!(summary, "retry 1/15 (next in 6.6s)");

    let running_state = ProviderRetryState {
        attempt: 4,
        max_attempts: 16,
        retry_at: now - std::time::Duration::from_millis(1_200),
        failure: "HTTP 503: overloaded".to_string(),
    };
    let running_summary = running_state.summary(now);
    assert_eq!(running_summary, "retry 3/15 (running for 1.2s)");
}


#[test]
fn completions_trigger_word_pins_suggestion_on_top() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let first = completions.first().expect("a suggestion row is present");
    assert_eq!(first.label, "/new");
    assert!(
        !first.description.is_empty(),
        "the suggestion must explain why the user is being steered"
    );
    // Accepting rewrites the whole input to the target command.
    assert_eq!(first.replace_start, 0);
    assert_eq!(first.replace_end, app.input.len());
    // No built-in starts with `/clear`, so the suggestion is the only row.
    assert_eq!(completions.len(), 1);
}


#[test]
fn completions_trigger_word_suggestion_precedes_prefix_matches() {
    // A trigger that also prefixes a real command must still pin its
    // suggestion first. `/re` is a shared prefix, not a trigger: normal
    // prefix completion with no suggestion. `/reset` is the full trigger.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(
        !app.completions().iter().any(|c| c.label == "/new"),
        "a partial trigger is prose-in-progress, not a suggestion"
    );

    app.input = "/reset".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/new"),
        "the suggestion pins on top even if a real command shares the prefix"
    );
}


#[test]
fn completions_continue_trigger_suggests_sessions() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/continue".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/sessions")
    );
}


#[test]
fn completions_settings_triggers_and_subcommands() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);

    // Typing /preferences steers to /settings
    app.input = "/preferences".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/settings")
    );

    // Typing /theme steers to /settings
    app.input = "/theme".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/settings")
    );

    // Typing /settings suggests /settings reload
    app.input = "/settings ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/settings reload"]);

    // Legacy /config <space> also suggests /settings reload
    app.input = "/config ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/settings reload"]);
}


#[test]
fn completions_subcommand_argument_never_triggers_suggestion() {
    // `clear` is a trigger word at the top level, but as a `/permissions`
    // argument it is a real subcommand and must not be steered away.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/permissions clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/permissions clear"]);
}


#[test]
fn completions_intent_keywords_suggest_canonical_command() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/timer".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/schedule"),
        "typing /timer should suggest /schedule"
    );

    // Check intent suggestion kind and doc
    let schedule_cand = completions.iter().find(|c| c.label == "/schedule").unwrap();
    assert!(matches!(
        schedule_cand.kind,
        crate::completion::CompletionItemKind::IntentSuggestion { .. }
    ));
    assert!(schedule_cand.doc.is_some());
    let doc = schedule_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/schedule");
    assert_eq!(doc.category.as_deref(), Some("Automation"));

    // /switch suggests /models and /master
    app.input = "/switch".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"/models"));
    assert!(labels.contains(&"/master"));
}


#[test]
fn completions_candidates_carry_rich_doc_for_inspector() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/models".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let models_cand = completions
        .iter()
        .find(|c| c.label == "/models")
        .expect("find /models");
    assert!(models_cand.doc.is_some());
    let doc = models_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/models");
    assert!(!doc.description.is_empty());
    assert!(!doc.usage.is_empty());
    assert!(!doc.examples.is_empty());
    assert_eq!(doc.category.as_deref(), Some("Model"));
}


#[test]
fn completions_returns_empty_when_input_does_not_trigger() {
    // Plain text without `@` or `/` produces no completions.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "hello world".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
    assert_eq!(app.completion_kind(), CompletionKind::None);
}


#[test]
fn completions_classifies_slash_input_as_slash_kind() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    assert!(completions.iter().any(|c| c.label == "/repeat"));
    // Slash candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }
}


#[test]
fn completions_yolo_subcommand_offers_on_off() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/yolo ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/yolo on") && labels.contains(&"/yolo off"),
        "expected both on/off subcommands, got {labels:?}"
    );
    // Candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }

    // Typing a prefix narrows the pair.
    app.input = "/yolo of".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/yolo off"]);

    // An unknown suffix dead-ends (no candidates, like any non-prefix).
    app.input = "/yolo x".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
}


#[test]
fn completions_expose_only_canonical_trust_subcommands() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);

    // Retired extension command is not suggested.
    app.input = "/extensions ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert!(completions.is_empty());

    // /trust <space> offers only the closed asset-domain grammar.
    app.input = "/trust ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "/trust all",
            "/trust mcp",
            "/trust skills",
            "/trust status",
            "/trust revoke"
        ]
    );

    // Removed subcommands do not reappear through prefix matching.
    app.input = "/trust w".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.is_empty());
}


#[test]
fn add_provider_row_opens_the_template_chooser() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_provider_template_chooser();
    assert!(app.active_modal() == Modal::ProviderTemplate);
    assert_eq!(app.template_choice, 0);
    // `↑/↓` wrap across the template list.
    let n = crate::PROVIDER_TEMPLATES.len();
    app.move_template_choice(false);
    assert_eq!(app.template_choice, n - 1, "wraps to the last template");
    app.move_template_choice(true);
    assert_eq!(app.template_choice, 0, "wraps back to the first");
}


#[test]
fn custom_provider_editor_opens_empty_on_name_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.custom_name = "stale".to_string();
    app.open_custom_provider_editor(openai_template());
    assert!(app.active_modal() == Modal::CustomProvider);
    assert_eq!(app.custom_field, 0, "opens on the Name field");
    assert!(app.custom_name.is_empty(), "buffers reset on open");
    assert!(
        app.input.is_empty(),
        "Name field borrows an empty input line"
    );
    // The template seeds the protocol and OpenAI model list.
    assert_eq!(app.custom_protocol_wire, "openai");
    assert!(app.custom_models.iter().any(|m| m == "gpt-5.5"));
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
}


#[test]
fn anthropic_template_seeds_the_claude_family_without_a_model_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(anthropic_template());
    assert_eq!(app.custom_protocol_wire, "anthropic");
    // The Claude family is seeded as the provider's model list…
    assert!(app.custom_models.len() > 1, "seeds multiple Claude models");
    assert!(app.custom_models.iter().any(|m| m.starts_with("claude-")));
    // …and there is no Model field (models are fixed by the template).
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
}


#[test]
fn antigravity_template_prefills_url_and_seeds_relay_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(antigravity_template());
    assert_eq!(app.custom_protocol_wire, "google");
    assert_eq!(
        app.custom_base_url,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    assert_eq!(app.custom_models, muta_providers::ANTIGRAVITY_OAUTH_MODELS);
    // No free-text Model field — the closed Gemini family is the seed.
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
    // Name and Token still start empty (the user supplies them).
    assert!(app.custom_name.is_empty());
    assert!(app.custom_token.is_empty());
}


#[test]
fn custom_provider_field_cycle_wraps_and_swaps_buffers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let custom_template = crate::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    app.open_custom_provider_editor(custom_template);
    // Fields: Name(0) / Base URL(1) / Token(2) / Model(3).
    let n = app.custom_fields.len() as u8;
    // Type a name, then advance: the name is stashed and the Base URL field
    // loads its (empty) buffer.
    app.input = "My Relay".to_string();
    app.cycle_custom_field(true);
    assert_eq!(app.custom_field, 1);
    assert_eq!(app.custom_name, "My Relay");
    assert!(app.input.is_empty(), "Base URL buffer is empty");
    // Wrap backward from Name (0) to the last field (Model).
    app.cycle_custom_field(false); // 1 -> 0
    assert_eq!(app.custom_field, 0);
    assert_eq!(app.input, "My Relay", "Name buffer reloads into the line");
    app.cycle_custom_field(false); // 0 -> n-1 (wrap)
    assert_eq!(app.custom_field, n - 1);
}


#[test]
fn custom_provider_model_filter_commits_and_offers_custom_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The real generic template: it exposes the Model field and seeds no
    // models, so the flow under test is exactly what ships.
    let free_model_template = crate::providers::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    app.open_custom_provider_editor(free_model_template);
    // The default model is the first candidate of the template's (OpenAI) protocol.
    assert!(
        app.custom_model_candidates()
            .contains(&app.custom_model.as_str())
    );
    // Focus the Model filter field (the last field) and type a known model.
    app.custom_field = app.custom_fields.len() as u8 - 1;
    assert_eq!(app.current_custom_field(), Some(crate::CustomField::Model));
    app.load_custom_field();
    app.input = "gpt-4o".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "gpt-4o");
    // A query matching nothing in the registry is still offered as a custom id.
    app.input = "my-private-model".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "my-private-model");
    // A query with spaces is automatically sanitized to use hyphens.
    app.input = "my custom private model".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "my-custom-private-model");
}


#[test]
fn custom_openai_template_submits_with_the_typed_model_and_url() {
    // End-to-end create flow for the generic template: fields Name/Base
    // URL/Token/Model, and the submitted `AddProvider` carries the typed
    // model id (not a seeded list) plus the relay endpoint.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let template = crate::providers::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    // The editor's visible fields include the Model filter field.
    assert_eq!(
        template.fields(),
        vec![
            crate::CustomField::Name,
            crate::CustomField::BaseUrl,
            crate::CustomField::Token,
            crate::CustomField::Model,
        ]
    );
    app.open_custom_provider_editor(template);
    app.custom_name = "WeChat".to_string();
    app.custom_base_url = "https://chatapi.weixin.qq.com/openai/v1/chat/completions".to_string();
    app.custom_token = "tok".to_string();
    // Focus the Model field, type the cased id, and commit it via the
    // suggestion commit (a cased id is offered as a custom value).
    app.custom_field = 3;
    app.load_custom_field();
    app.input = "GLM-5.2".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "GLM-5.2");

    // Submit: the request must carry the single typed model as the seeded
    // list, the template id, and the endpoint — a case-sensitive id travels
    // verbatim (the WeChat endpoint 400s on the lowercase spelling).
    app.stash_custom_field();
    let payload = serde_json::json!({
        "name": app.custom_name,
        "protocol": app.custom_protocol_wire,
        "base_url": app.custom_base_url,
        "models": [app.custom_model],
        "template_id": template.id,
    });
    assert_eq!(payload["models"][0], "GLM-5.2");
    assert_eq!(payload["template_id"], "custom-openai");
    assert_eq!(payload["protocol"], "openai");
    assert_eq!(
        payload["base_url"],
        "https://chatapi.weixin.qq.com/openai/v1/chat/completions"
    );
}


#[test]
fn completions_path_returns_top_level_for_bare_at() {
    // A bare `@` lists top-level entries only: the file plus the
    // synthesized top-level directory entry.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml", "src/main.rs", "README.md"], &["src"]);
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Path);

    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    // Dirs come first alphabetically, then files alphabetically.
    assert!(labels.contains(&"src/"));
    assert!(labels.contains(&"Cargo.toml"));
    assert!(labels.contains(&"README.md"));
    // No nested paths leak into the bare-`@` menu.
    assert!(!labels.iter().any(|l| l.contains("main.rs")));
    // The backend edit owns the whole mention, including the `@` trigger.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, 1);
        assert!(c.description.is_empty(), "path menu carries no description");
    }
}


#[test]
fn completions_path_descends_into_subdirectory() {
    // `@src/` triggers directory descend: only paths under `src/` match.
    let (mut app, _tmp) = app_in_tempdir(
        &["src/main.rs", "src/util/mod.rs", "tests/smoke.rs"],
        &["src", "src/util", "tests"],
    );
    app.input = "@src/".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"src/"));
    assert!(labels.contains(&"src/main.rs"));
    assert!(labels.contains(&"src/util/"));
    assert!(labels.contains(&"src/util/mod.rs"));
    // Nothing from `tests/` leaks in — descend is a prefix match.
    assert!(!labels.iter().any(|l| l.contains("tests")));
}


#[test]
fn completions_path_substring_match_picks_files_across_dirs() {
    // `@main` finds `src/main.rs` via substring match.
    let (mut app, _tmp) = app_in_tempdir(&["src/main.rs", "lib/other.rs"], &["src", "lib"]);
    app.input = "@main".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"src/main.rs"));
    assert!(!labels.iter().any(|l| l.contains("other.rs")));
}


#[test]
fn completions_path_skips_dotgit_directory() {
    // `.git/` is always excluded even though hidden files are kept.
    let (mut app, _tmp) = app_in_tempdir(
        &[".git/HEAD", ".git/config", "src/main.rs", ".env"],
        &[".git", "src"],
    );
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    // Hidden files like `.env` are listed; `.git/` and its contents are not.
    assert!(labels.contains(&".env"));
    assert!(labels.contains(&"src/"));
    assert!(!labels.iter().any(|l| l.starts_with(".git")));
}


#[test]
fn model_editor_owns_caret_only_for_provider_key_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::ModelEditor);
    app.editor_model_settings_only = false;
    app.editor_field = 0;
    assert_eq!(app.caret_owner(), CaretOwner::Modal);

    app.editor_model_settings_only = true;
    app.editor_field = 1;
    assert_eq!(app.caret_owner(), CaretOwner::None);
}


/// Deleting the highlighted row from the sessions picker must leave the cursor
/// on the **same line** (the next session slides up into the removed slot), not
/// jump back to the top. The `DeleteSelectedSession` event-loop arm does this
/// optimistically — it removes the row and clamps `modal_index` — so this pins
/// that core behaviour: a mid-list delete keeps the index, and a delete of the
/// last row clamps to the new last row rather than wrapping to 0.
#[test]
fn sessions_picker_delete_keeps_cursor_on_the_same_line() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.set_active_modal_for_test(Modal::Sessions);

    // Delete the row at index 2 (mid-list). The cursor must stay at 2 — now
    // pointing at "s3", which slid into the freed slot.
    app.modal_index = 2;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    let deleted = app.sessions_overview.remove(idx);
    assert_eq!(deleted.id, "s2");
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(app.modal_index, 2, "mid-list delete keeps the cursor put");
    assert_eq!(
        app.sessions_overview[app.modal_index].id, "s3",
        "the next session slid into the removed slot"
    );

    // Delete the now-last row (index 3 in the shrunken 4-row list, which holds
    // s4). The list then has 3 rows, so the cursor must clamp to index 2 (the
    // new last row), not jump to 0.
    app.modal_index = 3;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    app.sessions_overview.remove(idx);
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(
        app.modal_index, 2,
        "deleting the last row clamps to the new last row, not the top"
    );
}


/// Regression: after a delete the backend pushes a fresh `SessionsOverview`,
/// and the event loop used to treat *every* such push as an "open the picker"
/// request — resetting `modal_index` to 0 and `session_scroll` to 0. That
/// snapped the selection back to the top on each delete, undoing the optimistic
/// local removal. The refresh path must preserve the cursor/scroll when the
/// modal is already open, resetting only on a genuine open (closed → open).
#[test]
fn sessions_picker_data_refresh_does_not_reset_cursor_when_already_open() {
    // This mirrors the event-loop branch exactly: `opening` is true only when
    // the modal is not already Sessions.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.set_active_modal_for_test(Modal::Sessions);
    app.modal_index = 3;
    app.session_scroll = 2;

    // Simulate the refresh path (open_sessions signal + fresh overview) with
    // the modal ALREADY open: cursor and scroll must be preserved.
    let opening = app.active_modal() != Modal::Sessions; // false
    app.set_active_modal_for_test(Modal::Sessions);
    if opening {
        app.modal_index = 0;
        app.session_scroll = 0;
        app.session_modal_follow = true;
    }
    assert_eq!(app.modal_index, 3, "refresh while open keeps the cursor");
    assert_eq!(app.session_scroll, 2, "refresh while open keeps the scroll");

    // Now simulate opening from a different modal (the genuine-open case):
    // cursor and scroll reset to the top.
    app.set_active_modal_for_test(Modal::None);
    let opening = app.active_modal() != Modal::Sessions; // true
    app.set_active_modal_for_test(Modal::Sessions);
    if opening {
        app.modal_index = 0;
        app.session_scroll = 0;
        app.session_modal_follow = true;
    }
    assert_eq!(app.modal_index, 0, "a genuine open resets the cursor");
    assert_eq!(app.session_scroll, 0, "a genuine open resets the scroll");
}


// ---------------------------------------------------------------------------
// Unified surface router: transient stack, per-view drafts, queue hook,
// sub-layer pop, switcher filter.
// ---------------------------------------------------------------------------

#[test]
fn model_editor_esc_pops_back_to_its_picker() {
    // The surface stack replaces `editor_return_to`: an editor opened from
    // Models returns to Models; one opened from Connections returns to
    // Connections — the same editor, two parents, no hard-coding.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Models);
    app.push_transient_surface(crate::Modal::ModelEditor);
    app.pop_transient_surface();
    assert_eq!(app.active_modal(), crate::Modal::Models, "pops to Models");

    // From Connections: the same editor, a different pushed parent.
    app.open_panel(crate::surfaces::PanelId::Connections);
    app.push_transient_surface(crate::Modal::ModelEditor);
    app.pop_transient_surface();
    assert_eq!(
        app.active_modal(),
        crate::Modal::Connections,
        "pops to Connections"
    );
}
