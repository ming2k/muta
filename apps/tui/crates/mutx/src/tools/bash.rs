//! Presenter for `bash`.

use super::{ArgLayout, ResultKind, ToolPresenter, ToolView, truncate};

pub struct BashPresenter;

impl ToolPresenter for BashPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("command")
            .and_then(process_name)
            .map(|name| format!("Run {}", truncate(&name, 64)))
            .unwrap_or_else(|| "Run command".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Bash
    }

    fn arg_layout(&self) -> ArgLayout {
        ArgLayout::Command
    }
}

/// Best-effort process name for the compact header.
///
/// The expanded body already shows the exact `$ command` plus its output, so
/// repeating the invocation in the closed header wastes the disclosure. The
/// executable basename is both a quieter identity and the useful name an
/// operator would normally pass to `pkill` (`cargo`, not `cargo build`).
/// Leading shell-only setup segments such as `cd … &&` are skipped.
fn process_name(command: &str) -> Option<String> {
    for words in shell_segments(command) {
        let mut words = words.iter().skip_while(|word| is_assignment(word));
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

        let name = candidate
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(candidate);
        if !name.is_empty() {
            return Some(name.to_string());
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
    use super::process_name;

    #[test]
    fn process_name_is_the_executable_basename() {
        assert_eq!(process_name("cargo build"), Some("cargo".into()));
        assert_eq!(
            process_name("/opt/local/bin/muta-server --listen 8080"),
            Some("muta-server".into())
        );
        assert_eq!(
            process_name("'/opt/Long Path/worker' --serve"),
            Some("worker".into())
        );
    }

    #[test]
    fn process_name_skips_shell_setup_and_assignments() {
        assert_eq!(
            process_name("cd '/tmp/work tree' && RUST_LOG=debug cargo run"),
            Some("cargo".into())
        );
        assert_eq!(
            process_name("export MODE=test; ./target/debug/worker | tee worker.log"),
            Some("worker".into())
        );
    }
}
