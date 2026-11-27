#[path = "cli/check.rs"]
mod check;
#[path = "cli/flags.rs"]
mod flags;
#[path = "cli/help.rs"]
mod help;
#[path = "cli/hook.rs"]
mod hook;
#[path = "cli/land.rs"]
mod land;
#[path = "cli/local.rs"]
mod local;
#[path = "cli/request.rs"]
mod request;
#[path = "cli/run.rs"]
mod run;
#[path = "cli/serve.rs"]
mod serve;

use run::run;
use std::process::ExitCode;

fn main() -> ExitCode {
    let input: Vec<String> = std::env::args().skip(1).collect();
    // The hook runs on the host's hot path: no async runtime, one event, exit 0.
    if input.first().map(String::as_str) == Some("hook") {
        return hook::hook(input);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{}", error_line(&error.to_string(), &input));
            return ExitCode::FAILURE;
        }
    };
    let error = match runtime.block_on(run(input.clone())) {
        Ok(code) => return ExitCode::from(code),
        Err(error) => error.to_string(),
    };
    eprintln!("{}", error_line(&error, &input));
    ExitCode::FAILURE
}

/// A command's error for stderr: plain text for a person at a terminal,
/// one JSON line for a program or with --json (issue #291).
fn error_line(message: &str, input: &[String]) -> String {
    use std::io::IsTerminal;
    error_text(
        message,
        std::io::stderr().is_terminal() && !input.iter().any(|word| word == "--json"),
    )
}

fn error_text(message: &str, plain: bool) -> String {
    if plain {
        format!("langspan: {message}")
    } else {
        serde_json::json!({"level":"error","message":message}).to_string()
    }
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn an_error_is_plain_text_for_a_person_and_json_for_a_program() {
        assert_eq!(
            super::error_text("no \"x\" here; do y", true),
            "langspan: no \"x\" here; do y"
        );
        assert_eq!(
            super::error_text("no \"x\" here; do y", false),
            r#"{"level":"error","message":"no \"x\" here; do y"}"#
        );
    }
}
