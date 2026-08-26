//! Presenter for `execute_command` (and legacy persisted `bash` steps).

use super::{ArgLayout, ResultKind, ToolPresenter, ToolView, truncate};

pub struct ExecuteCommandPresenter;

impl ToolPresenter for ExecuteCommandPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("command")
            .and_then(command_summary)
            .map(|name| format!("Run {}", truncate(&name, 64)))
            .unwrap_or_else(|| "Run command".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Command
    }

    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::Command
    }

    /// Collapsed by default: the summary line ("Run cargo test · 0ms")
    /// covers the common case, and verbose command output otherwise dominates
    /// the transcript. Failures still force-expand (lifecycle rule in
    /// `step_interaction::default_tool_expanded`), and
    /// `[tui.default_expanded] execute_command = true` restores the old
    /// open-by-default behavior.
    fn default_expanded(&self) -> bool {
        false
    }
}

/// Human-readable command summary for the header.
///
/// Surfaces the executable and key arguments (e.g. `Run python3 test.py`,
/// `Run cargo test`) while skipping shell setup boilerplate (`cd ... &&`,
/// variable assignments). Multi-line scripts use their first executable segment.
fn command_summary(command: &str) -> Option<String> {
    for words in shell_segments(command) {
        let mut words = words.into_iter().skip_while(|word| is_assignment(word));
        let Some(candidate) = words.next() else {
            continue;
        };

        // These mutate the shell rather than starting the workload. Look in
        // the following `&&`, `;`, or newline-delimited segment instead.
        if matches!(
            candidate.as_str(),
            "cd" | "export" | "unset" | "set" | "source" | "." | "umask" | "ulimit"
        ) {
            continue;
        }

        let exec_name = candidate
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(&candidate)
            .to_string();

        if exec_name.is_empty() {
            continue;
        }

        let rest: Vec<String> = words.collect();
        if rest.is_empty() {
            return Some(exec_name);
        } else {
            return Some(format!("{} {}", exec_name, rest.join(" ")));
        }
    }
    None
}

/// Tokenize enough POSIX-shell syntax to locate command boundaries and the
/// first executable word. This is presentation-only: it deliberately does
/// not expand variables or substitutions, but it does preserve quoted paths
/// and avoids treating arguments after `&&`, `;`, pipes, or newlines as part
/// of the same command.
fn shell_segments(command: &str) -> Vec<Vec<String>> {
    #[derive(Clone, Copy)]
    enum Quote {
        Single,
        Double,
    }

    fn finish_word(word: &mut String, segment: &mut Vec<String>) {
        if !word.is_empty() {
            segment.push(std::mem::take(word));
        }
    }

    fn finish_segment(
        word: &mut String,
        segment: &mut Vec<String>,
        segments: &mut Vec<Vec<String>>,
    ) {
        finish_word(word, segment);
        if !segment.is_empty() {
            segments.push(std::mem::take(segment));
        }
    }

    let mut segments = Vec::new();
    let mut segment = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }

        match quote {
            Some(Quote::Single) => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some(Quote::Double) => match ch {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => word.push(ch),
            },
            None => match ch {
                '\'' => quote = Some(Quote::Single),
                '"' => quote = Some(Quote::Double),
                '\\' => escaped = true,
                '\n' | ';' | '|' | '&' => finish_segment(&mut word, &mut segment, &mut segments),
                ch if ch.is_whitespace() => finish_word(&mut word, &mut segment),
                _ => word.push(ch),
            },
        }
    }

    if escaped {
        word.push('\\');
    }
    finish_segment(&mut word, &mut segment, &mut segments);
    segments
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some('_') | Some('a'..='z') | Some('A'..='Z'))
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::command_summary;

    #[test]
    fn command_summary_includes_executable_and_args() {
        assert_eq!(
            command_summary("cargo build"),
            Some("cargo build".into())
        );
        assert_eq!(
            command_summary("/opt/local/bin/muta-server --listen 8080"),
            Some("muta-server --listen 8080".into())
        );
        assert_eq!(
            command_summary("'/opt/Long Path/worker' --serve"),
            Some("worker --serve".into())
        );
    }

    #[test]
    fn command_summary_skips_shell_setup_and_assignments() {
        assert_eq!(
            command_summary("cd '/tmp/work tree' && RUST_LOG=debug cargo run --release"),
            Some("cargo run --release".into())
        );
        assert_eq!(
            command_summary("export MODE=test; ./target/debug/worker | tee worker.log"),
            Some("worker".into())
        );
    }
}
