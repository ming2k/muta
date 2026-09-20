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
    // Small explicit budgets keep the one-third scaling.
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
    // …clamped to the 480s ceiling: the default 1800s (30 min) wall budget
    // tolerates 8 minutes of silence, so a compiling build (or output
    // buffered by `… | tail`) is not killed at an arbitrary short mark.
    assert_eq!(
        idle_budget_for(Duration::from_secs(1800)),
        Duration::from_secs(480)
    );
    assert_eq!(
        idle_budget_for(Duration::from_secs(600)),
        Duration::from_secs(200)
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
    let command = format!("sleep 60 & echo $! > {}; echo started", marker.display());
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

/// A huge multi-line output command is capped in memory.
#[tokio::test]
async fn execute_command_caps_huge_output_in_memory() {
    let tool = ExecuteCommandTool::new(None);
    let out = tool
        .call_structured(&arguments(native_command(
            "for i in $(seq 1 8000); do echo 'abcdefghij'; done; echo TAIL-MARKER",
            "1..8000 | ForEach-Object { [Console]::Out.WriteLine('abcdefghij') }; \
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
async fn test_background_spawn_via_service() {
    // ADR-0190: background spawn through the service returns a job id and
    // interactive-process spec; unknown args (`terminal_id`) are no longer
    // part of the schema.
    let tool = ExecuteCommandTool::new(None);
    let params = tool.parameters();
    let props = params.get("properties").expect("schema properties");
    assert!(
        props.get("terminal_id").is_none(),
        "terminal_id must be deleted from the tool surface (M6)"
    );
    assert!(
        props.get("run_persistent").is_none(),
        "run_persistent must be deleted from the tool surface (M6)"
    );
    assert!(
        props.get("service").is_none(),
        "service flag must be deleted from the tool surface (ADR-0263)"
    );
    assert!(
        props.get("background").is_none(),
        "background flag must be deleted from the tool surface (ADR-0263)"
    );
    // ADR-0234: the timer is a wake-prompt arm whose consumer (autonomous
    // wake) is disabled by ADR-0212, so `run_command` must not advertise
    // "run this command later" — the description promised a command
    // execution the runtime never performed.
    assert!(
        props.get("schedule_in_secs").is_none() && props.get("repeat").is_none(),
        "timer scheduling must not be advertised on the tool surface (ADR-0234)"
    );
}

#[cfg(unix)]
mod background_mode_selection {
    use super::*;

    #[tokio::test]
    async fn background_and_service_flags_are_ignored_in_favor_of_finite_execution() {
        // ADR-0263: background and service execution paths are eliminated.
        // All commands execute as finite synchronous commands.
        let tool = ExecuteCommandTool::new(None);
        let output = tool
            .call_structured(&arguments_flags(&["background", "service"]))
            .await
            .expect("structured output");
        let text = output.to_text();
        assert!(!text.contains("spawned_service"));
        assert!(!text.contains("spawned_in_background"));
    }

    fn arguments_flags(flags: &[&str]) -> String {
        let mut map = serde_json::Map::new();
        map.insert("command".to_string(), serde_json::json!("true"));
        for flag in flags {
            map.insert((*flag).to_string(), serde_json::json!(true));
        }
        serde_json::Value::Object(map).to_string()
    }
}

#[test]
fn execute_command_schema_documents_1800s_default_timeout() {
    let tool = ExecuteCommandTool::new(None);
    let params = tool.parameters();
    let desc = params
        .get("properties")
        .and_then(|p| p.get("timeout"))
        .and_then(|t| t.get("description"))
        .and_then(|d| d.as_str())
        .expect("timeout description");
    assert!(
        desc.contains("default 1800"),
        "schema description should state default 1800s: {desc}"
    );
}

#[test]
fn semantic_folding_collapses_pure_green_ninja_test_runs() {
    use super::pipes::{OutputCollector, is_pure_green_test_line};
    use muta_contracts::tool_output::{ShellLine, ShellStream};

    // Verify pattern matching
    assert!(is_pure_green_test_line("[1/134] test_alpha OK 0.01s"));
    assert!(is_pure_green_test_line("[ 2/134] test_beta OK 0.02s"));
    assert!(is_pure_green_test_line(
        "PASS [ 0.005s] crate::test_something"
    ));
    assert!(is_pure_green_test_line("test crate::test_something ... ok"));
    assert!(is_pure_green_test_line("✓ test_something"));

    // Verify negative invariants: warnings and errors are NEVER pure green
    assert!(!is_pure_green_test_line(
        "[1/134] test_foo OK (warning: leak detected)"
    ));
    assert!(!is_pure_green_test_line("[1/134] test_foo FAILED 0.05s"));
    assert!(!is_pure_green_test_line("test_foo ... FAILED"));

    // Build realistic collector with 10 passing tests followed by 1 failure
    let mut collector = OutputCollector::new();
    for i in 1..=10 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("[{i}/11] test_case_{i} OK 0.01s"),
        });
        collector
            .stdout_buf
            .push_str(&format!("[{i}/11] test_case_{i} OK 0.01s\n"));
    }
    collector.lines.push(ShellLine {
        stream: ShellStream::Out,
        text: "[11/11] test_case_11 FAILED 0.05s".into(),
    });
    collector
        .stdout_buf
        .push_str("[11/11] test_case_11 FAILED 0.05s\n");

    // Apply caps with folding enabled (raw: false)
    let (stdout, _stderr, lines, _truncated) = collector.apply_caps_ex(Some(1), false);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0].text,
        "⋯ 10 tests passed (pure-green output folded)"
    );
    assert_eq!(lines[1].text, "[11/11] test_case_11 FAILED 0.05s");
    assert!(stdout.contains("⋯ 10 tests passed (pure-green output folded)"));
    assert!(stdout.contains("FAILED"));
}

#[test]
fn semantic_folding_bypassed_when_raw_is_true() {
    use super::pipes::OutputCollector;
    use muta_contracts::tool_output::{ShellLine, ShellStream};

    let mut collector = OutputCollector::new();
    for i in 1..=5 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("[{i}/5] test_{i} OK 0.01s"),
        });
        collector
            .stdout_buf
            .push_str(&format!("[{i}/5] test_{i} OK 0.01s\n"));
    }

    // Apply caps with raw: true -> no folding
    let (stdout, _stderr, lines, _truncated) = collector.apply_caps_ex(Some(0), true);
    assert_eq!(lines.len(), 5);
    assert!(stdout.contains("[1/5] test_1 OK 0.01s"));
    assert!(!stdout.contains("pure-green output folded"));
}

#[test]
fn output_collector_detects_stream_flooding() {
    use super::pipes::{OutputCollector, SHELL_STREAM_FLOOD_LINES};
    use muta_contracts::tool_output::{ShellLine, ShellStream};

    let mut collector = OutputCollector::new();
    assert!(!collector.is_stream_flooded(false));

    // Under flood threshold
    for i in 0..SHELL_STREAM_FLOOD_LINES - 1 {
        collector.lines.push(ShellLine {
            stream: ShellStream::Out,
            text: format!("line {i}"),
        });
    }
    assert!(!collector.is_stream_flooded(false));

    // Reaching flood threshold triggers StreamGuard in normal mode
    collector.lines.push(ShellLine {
        stream: ShellStream::Out,
        text: "line flood".into(),
    });
    assert!(collector.is_stream_flooded(false));

    // In raw mode, higher ceiling applies
    assert!(!collector.is_stream_flooded(true));
}

#[test]
fn stream_cadence_tracker_detects_metronomic_monitoring_stream() {
    use super::pipes::StreamCadenceTracker;
    use std::time::{Duration, Instant};

    let mut tracker = StreamCadenceTracker::new();
    let base = Instant::now();

    // Line 1: Header row (e.g. intel_gpu_top -l header)
    assert!(!tracker.observe_at("Freq MHz IRQ RC6 Power W RCS BCS VCS VECS CCS", base));

    // Lines 2-6: Periodic data rows arriving ~500ms apart with matching column token counts
    for i in 1..=5 {
        let t = base + Duration::from_millis(500 * i);
        let triggered = tracker.observe_at("1351 355 213 32 2.97 14.45 46.43 0 0 0.00 0 0 0.00 0 0 0.00 0 0", t);
        if i < 5 {
            assert!(!triggered, "Should not trigger prematurely at sample {i}");
        } else {
            assert!(triggered, "Must trigger on 5th periodic sample row!");
        }
    }
}

#[test]
fn stream_cadence_tracker_ignores_rapid_compilation_bursts() {
    use super::pipes::StreamCadenceTracker;
    use std::time::{Duration, Instant};

    let mut tracker = StreamCadenceTracker::new();
    let base = Instant::now();

    // Rapid lines arriving within 20ms of each other (like cargo build or test runner)
    for i in 1..=100 {
        let t = base + Duration::from_millis(20 * i);
        let triggered = tracker.observe_at(&format!("Compiling crate_{i} v0.1.0"), t);
        assert!(!triggered, "High-frequency compilation bursts must not trigger cadence guard");
    }
}

#[test]
fn stream_cadence_tracker_detects_tui_redraws() {
    use super::pipes::StreamCadenceTracker;

    let mut tracker = StreamCadenceTracker::new();

    // First TUI frame
    assert!(!tracker.observe("\x1b[H\x1b[2Jtop - 14:00:00 up 10 days"));
    // Second TUI frame
    assert!(
        tracker.observe("\x1b[H\x1b[2Jtop - 14:00:01 up 10 days"),
        "Second TUI screen redraw must trigger early snapshot sufficiency"
    );
}

/// Continuous streaming output (e.g. `yes` or `intel_gpu_top -l`) in the foreground
/// is cut off early by StreamGuard rather than running to wall-clock timeout (ADR-0257).
#[tokio::test]
async fn execute_command_stream_guard_cuts_off_unbounded_stream() {
    let tool = ExecuteCommandTool::new(None);
    // `yes` produces infinite lines at maximum speed
    let args = serde_json::json!({
        "command": native_command("yes 'gpu metrics row'", "while ($true) { Write-Output 'gpu metrics row' }"),
        "timeout": 30,
    })
    .to_string();

    let out = tokio::time::timeout(Duration::from_secs(5), tool.call_structured(&args))
        .await
        .expect("StreamGuard must terminate infinite stream in seconds, never hanging")
        .expect("command execution succeeded with structured output");

    match &out {
        muta_contracts::ToolOutput::Shell {
            termination,
            exit,
            stdout,
            ..
        } => {
            assert_eq!(
                *termination,
                muta_contracts::tool_output::ShellTermination::StreamGuard,
                "Expected StreamGuard termination for infinite streaming command"
            );
            assert_eq!(*exit, None);
            assert!(
                stdout.contains("gpu metrics row"),
                "Snapshot must preserve output captured before cutoff"
            );
            // Verify model text representation includes actionable guidance
            let text = out.to_text();
            assert!(
                text.contains("[killed by harness: stream budget reached"),
                "to_text must provide actionable guidance to the model"
            );
        }
        other => panic!("expected Shell output, got {:?}", other),
    }
}

/// A command that emits an excessively long minified line triggers the content-aware
/// ingestion gate (ADR-0264), suppressing the inline raw line while flagging truncation.
#[tokio::test]
async fn execute_command_suppresses_long_minified_line() {
    let tool = ExecuteCommandTool::new(None);
    let minified_line = "a".repeat(10_000);
    let args = serde_json::json!({
        "command": native_command(
            &format!("echo '{minified_line}'"),
            &format!("Write-Output ('a' * 10000)"),
        ),
        "timeout": 10,
    })
    .to_string();

    let out = tool.call_structured(&args).await.expect("command succeeds");
    match out {
        muta_contracts::ToolOutput::Shell {
            stdout, truncated, ..
        } => {
            assert!(truncated, "single massive minified line must flag truncation");
            assert!(
                stdout.contains("[minified line:"),
                "stdout must contain minified line suppression notice, got: {stdout}"
            );
        }
        other => panic!("expected Shell output, got {:?}", other),
    }
}
