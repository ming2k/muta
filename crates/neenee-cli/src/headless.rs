use neenee_contracts::{
    AgentRequest, AgentResponse, PermissionDecision, PermissionRequest, RoundEvent,
    UserQuestionRequest,
};
use neenee_runtime::client::{self, AttachAction, Handshake};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

pub async fn run_headless(
    prompt: String,
    json: bool,
    project_override: Option<PathBuf>,
    autopilot: bool,
    _remote: Option<String>,
    _token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let info = client::ensure_daemon(&project_root).await?;
    if !client::versions_compatible(&info) {
        return Err(client::version_mismatch(&info).into());
    }

    let handshake = client::connect(&info, AttachAction::New).await?;
    let (tx, mut rx, session_id, _round_counter, _history, provider, model) = match handshake {
        Handshake::Attached {
            req_tx,
            resp_rx,
            session_id,
            round_counter,
            history,
            provider,
            model,
        } => (
            req_tx,
            resp_rx,
            session_id,
            round_counter,
            history,
            provider,
            model,
        ),
        Handshake::Pick(_) => {
            return Err("unexpected session pick list when creating fresh headless session".into());
        }
    };

    if autopilot {
        let _ = tx.send(AgentRequest::SlashCommand("/autopilot on".to_string()));
    }

    if json {
        let init_event = serde_json::json!({
            "type": "session_init",
            "session_id": session_id,
            "provider": provider,
            "model": model,
        });
        println!("{}", serde_json::to_string(&init_event)?);
        io::stdout().flush()?;
    }

    // Dispatch the prompt
    tx.send(AgentRequest::Chat {
        text: prompt,
        images: Vec::new(),
        sent_at_ms: None,
    })
    .map_err(|e| format!("could not send chat request: {e}"))?;

    let is_tty = io::stderr().is_terminal();
    let mut accumulated_text = String::new();

    while let Some(resp) = rx.recv().await {
        match resp {
            AgentResponse::Round {
                session_id: _,
                event,
            } => match event {
                RoundEvent::Text(delta) => {
                    if json {
                        accumulated_text.push_str(&delta);
                        let event_obj = serde_json::json!({
                            "type": "text_delta",
                            "delta": delta,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        accumulated_text.push_str(&delta);
                        print!("{delta}");
                        io::stdout().flush()?;
                    }
                }
                RoundEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "tool_call",
                            "id": id,
                            "name": name,
                            "arguments": arguments,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        if is_tty {
                            eprintln!("\n\x1b[36m[Tool Call]\x1b[0m {name}({arguments})");
                        } else {
                            eprintln!("\n[Tool Call] {name}({arguments})");
                        }
                        io::stderr().flush()?;
                    }
                }
                RoundEvent::ToolStream { id, stream } => {
                    if !json {
                        match stream {
                            neenee_contracts::ToolStream::Stdout(text) => {
                                let _ = io::stderr().write_all(text.as_bytes());
                                let _ = io::stderr().flush();
                            }
                            neenee_contracts::ToolStream::Stderr(text) => {
                                let _ = io::stderr().write_all(text.as_bytes());
                                let _ = io::stderr().flush();
                            }
                        }
                    } else {
                        let stream_obj = serde_json::json!({
                            "type": "tool_stream",
                            "id": id,
                        });
                        println!("{}", serde_json::to_string(&stream_obj)?);
                        io::stdout().flush()?;
                    }
                }
                RoundEvent::ToolResult {
                    id,
                    name,
                    output: _,
                    structured,
                    duration_ms,
                } => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "tool_result",
                            "id": id,
                            "name": name,
                            "duration_ms": duration_ms,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        let is_error = matches!(
                            structured,
                            neenee_contracts::ToolOutput::Error { .. }
                                | neenee_contracts::ToolOutput::PermissionDenied { .. }
                        ) || match &structured {
                            neenee_contracts::ToolOutput::Shell { exit, .. } => {
                                exit.is_some_and(|code| code != 0)
                            }
                            _ => false,
                        };
                        let status_label = if !is_error {
                            if is_tty {
                                "\x1b[32mcompleted\x1b[0m"
                            } else {
                                "completed"
                            }
                        } else {
                            if is_tty {
                                "\x1b[31mfailed\x1b[0m"
                            } else {
                                "failed"
                            }
                        };
                        eprintln!("[Tool {status_label}] {name} ({duration_ms}ms)");
                        io::stderr().flush()?;
                    }
                }
                RoundEvent::PermissionRequest(req) => {
                    handle_permission_request(&tx, req, autopilot, is_tty).await?;
                }
                RoundEvent::UserQuestionRequest(req) => {
                    handle_user_question_request(&tx, req, is_tty).await?;
                }
                RoundEvent::RoundCompleted(summary) => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "round_completed",
                            "output_tokens": summary.output_tokens,
                            "duration_ms": summary.duration_ms,
                            "generation_ms": summary.generation_ms,
                            "response": accumulated_text,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                        io::stdout().flush()?;
                    } else {
                        if !accumulated_text.ends_with('\n') {
                            println!();
                        }
                        io::stdout().flush()?;
                    }
                    return Ok(());
                }
                RoundEvent::Error(err) => {
                    if json {
                        let event_obj = serde_json::json!({
                            "type": "error",
                            "error": err,
                        });
                        println!("{}", serde_json::to_string(&event_obj)?);
                    } else {
                        eprintln!("\nneenee: error: {err}");
                    }
                    return Err(err.into());
                }
                _ => {}
            },
            AgentResponse::Error(err) => {
                if json {
                    let event_obj = serde_json::json!({
                        "type": "error",
                        "error": err,
                    });
                    println!("{}", serde_json::to_string(&event_obj)?);
                } else {
                    eprintln!("\nneenee: error: {err}");
                }
                return Err(err.into());
            }
            AgentResponse::Exit => {
                return Ok(());
            }
            _ => {}
        }
    }

    Ok(())
}

async fn handle_permission_request(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    req: PermissionRequest,
    autopilot: bool,
    is_tty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if autopilot {
        let _ = tx.send(AgentRequest::PermissionReply {
            request_id: req.id,
            decision: PermissionDecision::Once,
            parent_call_id: None,
        });
        return Ok(());
    }

    if !is_tty {
        eprintln!(
            "neenee: permission requested for tool '{}' in non-interactive mode; rejecting (use -y/--autopilot to auto-approve).",
            req.tool
        );
        let _ = tx.send(AgentRequest::PermissionReply {
            request_id: req.id,
            decision: PermissionDecision::Reject,
            parent_call_id: None,
        });
        return Ok(());
    }

    eprintln!(
        "\n\x1b[33m[Permission Request]\x1b[0m Tool: {} ({})\nDetails: {}",
        req.tool, req.scope, req.description
    );
    eprint!("Allow execution? [y]es, [a]lways, [n]o: ");
    io::stderr().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();

    let decision = match answer.as_str() {
        "y" | "yes" => PermissionDecision::Once,
        "a" | "always" => PermissionDecision::Always,
        _ => PermissionDecision::Reject,
    };

    let _ = tx.send(AgentRequest::PermissionReply {
        request_id: req.id,
        decision,
        parent_call_id: None,
    });
    Ok(())
}

async fn handle_user_question_request(
    tx: &tokio::sync::mpsc::UnboundedSender<AgentRequest>,
    req: UserQuestionRequest,
    is_tty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut all_answers = Vec::new();

    for q in &req.questions {
        if !is_tty {
            let default_answer = q
                .options
                .first()
                .map(|opt| opt.label.clone())
                .unwrap_or_default();
            all_answers.push(vec![default_answer]);
            continue;
        }

        eprintln!("\n\x1b[35m[Question]\x1b[0m {}", q.question);
        for (i, opt) in q.options.iter().enumerate() {
            if let Some(desc) = &opt.description {
                eprintln!("  {}. {} ({desc})", i + 1, opt.label);
            } else {
                eprintln!("  {}. {}", i + 1, opt.label);
            }
        }
        eprint!("Your choice (number or write-in): ");
        io::stderr().flush()?;

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let choice = line.trim();

        if let Ok(idx) = choice.parse::<usize>()
            && idx > 0
            && idx <= q.options.len()
        {
            all_answers.push(vec![q.options[idx - 1].label.clone()]);
            continue;
        }
        all_answers.push(vec![choice.to_string()]);
    }

    let _ = tx.send(AgentRequest::UserQuestionReply {
        request_id: req.id,
        answers: all_answers,
        parent_call_id: None,
    });
    Ok(())
}
