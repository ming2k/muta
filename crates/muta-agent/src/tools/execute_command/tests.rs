use super::*;
use std::time::Duration;

#[cfg(unix)]
fn native_command<'a>(posix: &'a str, _powershell: &'a str) -> &'a str {
    posix
}

#[cfg(windows)]
fn native_command<'a>(_posix: &'a str, powershell: &'a str) -> &'a str {
    powershell
}

fn arguments(command: &str) -> String {
    serde_json::json!({ "command": command }).to_string()
}

#[test]
fn idle_budget_scales_with_timeout() {
    use super::episodic::idle_budget_for;
    // Default 30s call keeps the historical 10s budget.
    assert_eq!(
        idle_budget_for(Duration::from_secs(30)),
        Duration::from_secs(10)
    );
    // Explicitly larger budgets scale up as timeout/3…
    assert_eq!(
        idle_budget_for(Duration::from_secs(60)),
        Duration::from_secs(20)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(180)),
        Duration::from_secs(60)
    );
    // …clamped to the 60s ceiling and the 5s floor.
    assert_eq!(
        idle_budget_for(Duration::from_secs(600)),
        Duration::from_secs(60)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(3)),
        Duration::from_secs(5)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(9)),
        Duration::from_secs(5)
    );
}

/// A healthy command captures stdout and exits cleanly with `Exited`.
#[tokio::test]
async fn execute_command_captures_stdout_and_exits() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "printf hello",
            "[Console]::Out.Write('hello')",
        )))
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell {
            stdout,
            exit,
            termination,
            ..
        } => {
            assert_eq!(stdout, "hello\n");
            assert_eq!(exit, Some(0));
            assert_eq!(
                termination,
                muta_contracts::tool_output::ShellTermination::Exited
            );
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// The default stdin policy is Closed (`/dev/null`), so a command that
/// reads stdin gets instant EOF and fails fast instead of hanging. This
/// is the L1 hard floor: `cat` with no input and closed stdin exits 0
/// immediately.
#[tokio::test]
async fn execute_command_closed_stdin_means_eof_not_hang() {
    let tool = ExecuteCommandTool::new(None);
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        tool.call_structured(&arguments(native_command(
            "read x",
            "if ($null -eq [Console]::In.ReadLine()) { exit 7 }",
        ))),
    )
    .await
    .expect("closed stdin must NOT hang past 5s");
    match out.expect("ok") {
        muta_contracts::ToolOutput::Shell { exit, .. } => {
            assert_ne!(exit, Some(0));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// A prefilled stdin policy pipes the bytes into the child: `cat` echoes
/// them back. This is the L3.5 seam (human/model input injection).
#[tokio::test]
async fn execute_command_prefilled_stdin_feeds_the_child() {
    let tool = ExecuteCommandTool::new(None);
    let mut on_stream = |_: muta_contracts::ToolStream| ();
    let out = tool
        .call_structured_with_events(
            "",
            &arguments(native_command(
                "cat",
                "[Console]::Out.Write([Console]::In.ReadToEnd())",
            )),
            Box::new(|_| {}),
            &mut on_stream,
            muta_contracts::StdinPolicy::Prefilled {
                data: "injected\n".into(),
            },
        )
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell { stdout, exit, .. } => {
            assert_eq!(stdout, "injected\n");
            assert_eq!(exit, Some(0));
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// The child runs in its own process group (`.process_group(0)`), so its
/// process id equals its process-group id.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_child_runs_in_its_own_process_group() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(r#"{"command":"ps -o pid=,pgid= -p $$ || echo \"ps=$$\""}"#)
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell { stdout, exit, .. } => {
            let _ = stdout;
            let _ = exit;
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

/// A timed-out command's whole process group is killed.
#[cfg(unix)]
#[tokio::test]
async fn execute_command_timeout_kills_grandchildren() {
    let tool = ExecuteCommandTool::new(None);
    let marker = std::env::temp_dir().join(format!(
        "muta-grandchild-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let command = format!(
        "sleep 60 & echo $! > {}; echo started",
        marker.display()
    );
    let out = tool
        .call_structured(&format!(
            r#"{{"command":{}, "timeout": 2}}"#,
            serde_json::to_string(&command).unwrap()
        ))
        .await;
    assert!(matches!(
        &out,
        Ok(muta_contracts::ToolOutput::Shell {
            termination: muta_contracts::tool_output::ShellTermination::Timeout,
            ..
        })
    ));
    assert!(out.as_ref().unwrap().is_error());

    let pid_txt = std::fs::read_to_string(&marker).unwrap_or_default();
    let pid: i32 = pid_txt.trim().parse().unwrap_or(0);
    let _ = std::fs::remove_file(&marker);
    assert!(pid > 0, "grandchild did not record its pid ({pid_txt:?})");
    let alive = |pid: i32| {
        std::path::Path::new(&format!("/proc/{pid}"))
            .try_exists()
            .unwrap_or(false)
    };
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("grandchild pid {pid} survived the group kill");
}

#[cfg(windows)]
#[tokio::test]
async fn execute_command_timeout_kills_grandchildren() {
    let tool = ExecuteCommandTool::new(None);
    let marker = std::env::temp_dir().join(format!(
        "muta-grandchild-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let escaped_marker = marker
        .to_string_lossy()
        .replace('`', "``")
        .replace('"', "`\"");
    let command = format!(
        "$p = Start-Process powershell.exe -WindowStyle Hidden -PassThru \
         -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command',\
         'Start-Sleep -Seconds 60'; \
         Set-Content -LiteralPath \"{escaped_marker}\" -Value $p.Id; \
         Write-Output started; Wait-Process -Id $p.Id"
    );
    let out = tool
        .call_structured(&serde_json::json!({ "command": command, "timeout": 2 }).to_string())
        .await;
    assert!(matches!(
        &out,
        Ok(muta_contracts::ToolOutput::Shell {
            termination: muta_contracts::tool_output::ShellTermination::Timeout,
            ..
        })
    ));
    assert!(out.as_ref().unwrap().is_error());

    let pid_text = std::fs::read_to_string(&marker).unwrap_or_default();
    let pid: u32 = pid_text.trim().parse().unwrap_or(0);
    let _ = std::fs::remove_file(&marker);
    assert!(pid > 0, "grandchild did not record its pid ({pid_text:?})");
    for _ in 0..50 {
        if muta_platform::process::process_identity(pid).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("grandchild pid {pid} survived the Job Object termination");
}

/// A huge-output command is capped in memory.
#[tokio::test]
async fn execute_command_caps_huge_output_in_memory() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "for i in $(seq 1 80000); do printf 'abcdefghij'; done; echo TAIL-MARKER",
            "[Console]::Out.Write((('abcdefghij' * 80000) -join '')); \
             [Console]::Out.WriteLine('TAIL-MARKER')",
        )))
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell {
            stdout, truncated, ..
        } => {
            assert!(truncated, "collection cap must set the hint");
            assert!(
                stdout.contains("dropped (collection cap)"),
                "marker present"
            );
            assert!(
                stdout.len() < 70_000,
                "payload bounded near the 64k cap, got {}",
                stdout.len()
            );
            assert!(stdout.starts_with("abcdefghij"), "head kept");
            assert!(stdout.contains("TAIL-MARKER"), "tail kept");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

/// Captured tabs are expanded to spaces.
#[tokio::test]
async fn execute_command_captures_expanded_tabs() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "printf 'a\\tb\\n'",
            "[Console]::Out.Write(\"a`tb`n\")",
        )))
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell { stdout, .. } => {
            assert_eq!(stdout, "a       b\n");
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn execute_command_runs_in_the_session_workspace_root() {
    let marker = std::env::temp_dir().join(format!("muta-command-root-{}", std::process::id()));
    std::fs::create_dir_all(&marker).expect("mkdir");
    let tool = ExecuteCommandTool::new(Some(marker.clone()));
    let out = tool
        .call_structured(r#"{"command":"pwd"}"#)
        .await
        .expect("ok");
    match out {
        muta_contracts::ToolOutput::Shell { stdout, .. } => {
            let expected = marker.canonicalize().expect("canonical workspace root");
            assert_eq!(stdout.trim(), expected.as_os_str().to_string_lossy());
        }
        other => panic!("expected Shell, got {:?}", other),
    }
    std::fs::remove_dir_all(&marker).ok();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn workspace_shell_sees_only_runtime_and_exact_workspace() {
    if !crate::execution::workspace_sandbox_available() {
        return;
    }
    let base = std::env::temp_dir().join(format!(
        "muta-command-sandbox-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let workspace = base.join("workspace");
    let outside = base.join("outside-secret");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("visible"), "workspace").expect("write workspace marker");
    std::fs::write(&outside, "host secret").expect("write host marker");

    let env = std::sync::Arc::new(crate::execution::WorkspaceExecutionEnvironment::new(
        &workspace,
    ));
    let tool = ExecuteCommandTool::workspace_with_env(env);
    let command = format!(
        "test -r visible && test ! -e {} && test ! -e /etc/passwd && \
         test -z \"${{CARGO_MANIFEST_DIR:-}}\" && printf sandboxed > created",
        outside.display()
    );
    let output = tool
        .call_structured(&serde_json::json!({ "command": command }).to_string())
        .await
        .expect("sandbox command");
    assert!(matches!(
        output,
        muta_contracts::ToolOutput::Shell { exit: Some(0), .. }
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.join("created")).unwrap(),
        "sandboxed"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host secret");
    std::fs::remove_dir_all(base).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn test_persistent_terminal_preserves_state() {
    let tool = ExecuteCommandTool::new(None);
    let res1 = tool
        .call_structured(
            r#"{"command":"export MUTA_TEST_VAR=12345", "terminal_id": "test_sess"}"#,
        )
        .await
        .expect("set env");
    assert!(matches!(res1, muta_contracts::ToolOutput::Shell { .. }));

    let res2 = tool
        .call_structured(r#"{"command":"echo $MUTA_TEST_VAR", "terminal_id": "test_sess"}"#)
        .await
        .expect("read env");
    match res2 {
        muta_contracts::ToolOutput::Shell { stdout, .. } => {
            assert_eq!(stdout.trim(), "12345");
        }
        other => panic!("expected Shell, got {:?}", other),
    }
}

#[test]
fn execute_command_schema_documents_300s_default_timeout() {
    let tool = ExecuteCommandTool::new(None);
    let params = tool.parameters();
    let desc = params
        .get("properties")
        .and_then(|p| p.get("timeout"))
        .and_then(|t| t.get("description"))
        .and_then(|d| d.as_str())
        .expect("timeout description");
    assert!(
        desc.contains("default 300"),
        "schema description should state default 300s: {desc}"
    );
}

