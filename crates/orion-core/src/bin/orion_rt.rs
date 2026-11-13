//! `orion-rt`: the command an agent uses to drive other agents.
//!
//! The runtime daemon owns the terminals and answers verbs over the runtime
//! socket (see [`orion_core::runtime::daemon`]). This binary is a thin client
//! of those verbs, so an agent running in one terminal can start, steer, wait
//! on, read from, and end another.
//!
//! # Verbs
//!
//! Each command is one request to the daemon. The request carries the id the
//! daemon answers with, so the CLI waits for exactly its own answer.
//!
//! | command | daemon verb | answer the CLI uses |
//! |---|---|---|
//! | `spawn-agent` (`run`) | `terminal-spawn` | `ok`, `terminal_id` |
//! | `list` (`ls`) | `terminal-list` | `terminals` |
//! | `attach` | (pushed `pty-data` frames) | `data` |
//! | `read` | `terminal-read` | `text` |
//! | `send` | `terminal-send` | `ok` |
//! | `wait` | `terminal-wait` | `done`, `status`, `exited`, `address` |
//! | `kill` | `terminal-kill` | `ok` |
//! | `report` | `terminal-report` | `ok` |
//!
//! `--skill` prints the guide an agent reads before it delegates work; it is
//! part of the deliverable, not a comment. `--help` prints the usage.

use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use orion_core::agents::LaunchSpec;
use orion_core::protocol::{AgentKind, ReportedStatus};
use orion_core::runtime::{current_session, socket_path, RuntimeClient, RuntimeStream};
use serde_json::{json, Value};

/// The agent kinds `--agent` accepts, by the name the client uses.
const AGENT_KINDS: &[&str] = &[
    "claude", "codex", "agy", "opencode", "cursor", "grok", "omp", "omo", "pi", "hermes", "devin",
];

/// The terminal the daemon stamped into this process, when the CLI runs inside
/// one of the daemon's terminals.
const TERMINAL_ID_ENV: &str = "RUNTIME_TERMINAL_ID";

/// The usage `--help` prints and a wrong command is answered with.
const USAGE: &str = "\
Usage: orion-rt <command> [arguments]

Commands:
  spawn-agent [--bg] [--name NAME] [--cwd DIR] [--agent KIND] [-- <program> <args>...]
      Start a terminal. `--agent` starts an agent CLI by kind; without it, give
      a program after `--`. Prints the terminal id, then its address.
  list
      List the terminals: id, name, parent, status, cwd.
  attach <id>
      Stream a terminal's new output until it ends.
  read <id> [--lines N]
      Print the last N lines of a terminal's output. Default 40.
  send <id> <text>
      Write text to a terminal's stdin, then press Enter.
  wait <id> [--timeout SECS]
      Wait for a report or an exit. Default 30 seconds.
  kill <id>
      End a terminal and its children.
  report <id> <working|done|blocked|error>
      Record what a program reported about itself.
  skill
      Print the agent-facing guide.

Aliases: `run` is `spawn-agent`, `ls` is `list`.

Options:
  --skill   Print the agent-facing guide and exit.
  --help    Print this help and exit.
";

/// The guide `--skill` prints: how an agent delegates to another agent.
///
/// The copy in `docs/` says the same thing; keep the two identical.
const SKILL: &str = include_str!("../../../../docs/agent-runtime.md");

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches one command.
async fn run() -> Result<ExitCode, String> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let rest: Vec<String> = args.collect();

    match command.as_str() {
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            flush_stdout()?;
            Ok(ExitCode::SUCCESS)
        }
        "--skill" | "skill" => {
            print!("{SKILL}");
            flush_stdout()?;
            Ok(ExitCode::SUCCESS)
        }
        "spawn-agent" | "run" => spawn_verb(&rest).await,
        "list" | "ls" => list_verb().await,
        "attach" => attach_verb(&rest).await,
        "read" => read_verb(&rest).await,
        "send" => send_verb(&rest).await,
        "wait" => wait_verb(&rest).await,
        "kill" => kill_verb(&rest).await,
        "report" => report_verb(&rest).await,
        other => Err(format!("Unknown command: {other}\n\n{USAGE}")),
    }
}

/// Connects to the runtime, or explains how to start one.
async fn connect() -> Result<RuntimeClient, String> {
    let session = current_session();
    let stream = RuntimeStream::connect(&session).await.map_err(|error| {
        format!(
            "No runtime is running. Start one with `orion daemon`.\n(session: {session}; socket: {}; {error})",
            socket_path(&session).display()
        )
    })?;
    RuntimeClient::new(Box::new(stream))
        .map_err(|error| format!("The runtime did not complete the handshake: {error}"))
}

/// Sends one request and turns a refusal into a plain error.
fn ask(client: &RuntimeClient, request: Value) -> Result<Value, String> {
    let answer = client
        .request(request)
        .map_err(|error| format!("The runtime closed the connection: {error}"))?;
    if answer.get("type").and_then(Value::as_str) == Some("error") {
        let message = answer
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the request was refused");
        return Err(format!("The runtime refused the request: {message}"));
    }
    Ok(answer)
}

/// The address a caller writes down to come back to a terminal.
fn address(session: &str, id: &str) -> String {
    format!("{}:{}", socket_path(session).display(), id)
}

/// The terminal this process runs inside, as the daemon stamped it.
fn current_terminal_id() -> Value {
    match std::env::var(TERMINAL_ID_ENV) {
        Ok(id) if !id.trim().is_empty() => Value::String(id),
        _ => Value::Null,
    }
}

/// The runtime's record of one terminal, or a plain error naming the id.
fn terminal_info(client: &RuntimeClient, id: &str) -> Result<Value, String> {
    let answer = ask(client, json!({ "type": "terminal-list" }))?;
    let found = answer
        .get("terminals")
        .and_then(Value::as_array)
        .and_then(|terminals| {
            terminals
                .iter()
                .find(|terminal| terminal.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
        });
    found.ok_or_else(|| {
        format!("No such terminal: {id}. Use `orion-rt list` to see what this runtime holds.")
    })
}

/// What a spawn starts.
enum SpawnTarget {
    /// An agent CLI by kind. The runtime resolves the binary.
    Agent(String),
    /// A program and the arguments it gets.
    Program { program: String, args: Vec<String> },
}

/// What a spawn asks for.
struct SpawnRequest {
    target: SpawnTarget,
    name: Option<String>,
    cwd: Option<String>,
    background: bool,
}

/// `spawn-agent`: starts a terminal and hands back its id.
async fn spawn_verb(args: &[String]) -> Result<ExitCode, String> {
    let request = parse_spawn_request(args)?;
    let (program, program_args) = match &request.target {
        SpawnTarget::Agent(kind) => resolve_agent(kind)?,
        SpawnTarget::Program { program, args } => (program.clone(), args.clone()),
    };
    let cwd = match &request.cwd {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()
            .map_err(|error| format!("Cannot read the current directory: {error}"))?
            .to_string_lossy()
            .into_owned(),
    };

    let session = current_session();
    let client = connect().await?;
    let answer = ask(
        &client,
        json!({
            "type": "terminal-spawn",
            "program": program,
            "args": program_args,
            "cwd": cwd,
            "name": request.name,
            "parent_id": current_terminal_id(),
            "outlive_parent": false,
        }),
    )?;

    if answer.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the runtime did not start the program");
        return Err(format!("The runtime did not start the program: {message}"));
    }
    let id = answer
        .get("terminal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "The runtime did not return a terminal id.".to_string())?;

    println!("{id}");
    if !request.background {
        println!("address {}", address(&session, id));
    }
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// Reads the spawn flags and the program after `--`.
///
/// Everything after `--` belongs to the program, so its own flags reach it.
fn parse_spawn_request(args: &[String]) -> Result<SpawnRequest, String> {
    let mut name = None;
    let mut cwd = None;
    let mut agent = None;
    let mut background = false;
    let mut command: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bg" => background = true,
            "--name" => {
                index += 1;
                name = Some(flag_value(args, index, "--name")?);
            }
            "--cwd" => {
                index += 1;
                cwd = Some(flag_value(args, index, "--cwd")?);
            }
            "--agent" => {
                index += 1;
                agent = Some(flag_value(args, index, "--agent")?);
            }
            "--" => {
                command = args[index + 1..].to_vec();
                break;
            }
            other => {
                return Err(format!(
                    "Unknown option for spawn-agent: {other}\n\n{USAGE}"
                ))
            }
        }
        index += 1;
    }

    let target = match (&agent, command.first()) {
        (Some(kind), None) => SpawnTarget::Agent(kind.clone()),
        (Some(_), Some(_)) => {
            return Err("Give either --agent or a program after `--`, not both.".to_string())
        }
        (None, Some(program)) => SpawnTarget::Program {
            program: program.clone(),
            args: restore_switches(&command[1..]),
        },
        (None, None) => {
            return Err(format!(
            "`spawn-agent` needs a program after `--`, or an agent kind with `--agent`.\n\n{USAGE}"
        ))
        }
    };

    Ok(SpawnRequest {
        target,
        name,
        cwd,
        background,
    })
}

/// The environment variable a POSIX-path shell sets for the programs it starts.
const POSIX_SHELL_ENV: &str = "MSYSTEM";

/// Puts back the switches a POSIX-path shell rewrote.
///
/// Git Bash and MSYS2 turn a single-letter switch such as `/C` into `C:/`
/// before a native program sees it. A program that gets `C:/` instead of `/C`
/// starts in the wrong mode and waits for a person, so the CLI restores the
/// one shape it can still recognise: a drive letter, a colon, and nothing
/// else. It says so on stderr, because a caller who meant a real drive path
/// deserves to know.
fn restore_switches(args: &[String]) -> Vec<String> {
    if std::env::var_os(POSIX_SHELL_ENV).is_none() {
        return args.to_vec();
    }
    restore_posix_switches(args)
}

/// The switches a POSIX-path shell rewrote, put back.
fn restore_posix_switches(args: &[String]) -> Vec<String> {
    let mut restored = Vec::with_capacity(args.len());
    let mut repaired: Vec<(String, String)> = Vec::new();
    for arg in args {
        match rewritten_switch(arg) {
            Some(switch) => {
                repaired.push((arg.clone(), switch.clone()));
                restored.push(switch);
            }
            None => restored.push(arg.clone()),
        }
    }
    for (was, now) in repaired {
        eprintln!("note: the shell turned {was} into a drive path; the program gets {now}");
    }
    restored
}

/// The switch a POSIX-path shell rewrote, if this argument is one.
fn rewritten_switch(arg: &str) -> Option<String> {
    let mut chars = arg.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    if chars.next()? != ':' {
        return None;
    }
    if chars.next()? != '/' {
        return None;
    }
    if chars.next().is_some() {
        return None;
    }
    Some(format!("/{letter}"))
}

/// The value of a flag that needs one.
fn flag_value(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value.\n\n{USAGE}"))
}

/// Turns an agent kind into the program and arguments the runtime starts.
fn resolve_agent(name: &str) -> Result<(String, Vec<String>), String> {
    let kind = AgentKind::from_wire(name).ok_or_else(|| {
        format!(
            "Unknown agent kind: {name}\nKnown kinds: {}",
            AGENT_KINDS.join(", ")
        )
    })?;
    let spec = LaunchSpec::for_agent(kind, &[], None, false)
        .map_err(|error| format!("{error}\nInstall the agent, or name a program after `--`."))?;
    Ok((spec.program.to_string_lossy().into_owned(), spec.args))
}

/// `list`: one line per terminal.
async fn list_verb() -> Result<ExitCode, String> {
    let client = connect().await?;
    let answer = ask(&client, json!({ "type": "terminal-list" }))?;
    let Some(terminals) = answer.get("terminals").and_then(Value::as_array) else {
        return Err("The runtime did not list its terminals.".to_string());
    };
    if terminals.is_empty() {
        println!("no terminals");
        flush_stdout()?;
        return Ok(ExitCode::SUCCESS);
    }
    println!("id\tname\tparent\tstatus\tcwd");
    for terminal in terminals {
        println!("{}", terminal_row(terminal));
    }
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// One tab-separated `list` line: id, name, parent, status, cwd.
fn terminal_row(terminal: &Value) -> String {
    let field = |key: &str| {
        terminal
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string()
    };
    let status = terminal
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            terminal
                .get("exited")
                .and_then(Value::as_i64)
                .map(|code| format!("exited({code})"))
        })
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{}\t{}\t{}\t{}\t{}",
        field("id"),
        field("name"),
        field("parent_id"),
        status,
        field("cwd")
    )
}

/// What `attach` waits for.
enum AttachEvent {
    Output(String),
    Ended,
}

/// `attach`: prints the terminal's new output until it ends.
async fn attach_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`attach` needs a terminal id.\n\n{USAGE}"))?;
    let client = connect().await?;
    let info = terminal_info(&client, id)?;
    if let Some(code) = info.get("exited").and_then(Value::as_i64) {
        println!(
            "the terminal has already ended (exit {code}); use `orion-rt read {id} --lines 200` \
             to see what it said."
        );
        flush_stdout()?;
        return Ok(ExitCode::SUCCESS);
    }

    // The daemon pushes every terminal's output to every connection; keep the
    // frames that belong to this one.
    let (sender, receiver) = mpsc::channel::<AttachEvent>();
    let pane_id = id.clone();
    let stop = client.on_frame(Arc::new(move |frame: Value| {
        if frame.get("pane_id").and_then(Value::as_str) != Some(pane_id.as_str()) {
            return;
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("pty-data") => {
                if let Some(data) = frame.get("data").and_then(Value::as_str) {
                    let _ = sender.send(AttachEvent::Output(data.to_string()));
                }
            }
            Some("pty-exit") => {
                let _ = sender.send(AttachEvent::Ended);
            }
            _ => {}
        }
    }));

    for event in receiver {
        match event {
            AttachEvent::Output(text) => {
                print!("{text}");
                flush_stdout()?;
            }
            AttachEvent::Ended => {
                eprintln!("terminal {id} ended");
                break;
            }
        }
    }
    drop(stop);
    Ok(ExitCode::SUCCESS)
}

/// `read`: the last N lines the terminal printed.
async fn read_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`read` needs a terminal id.\n\n{USAGE}"))?;
    let mut lines = 40usize;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--lines" => {
                index += 1;
                lines = flag_value(args, index, "--lines")?
                    .parse()
                    .map_err(|_| "--lines must be a whole number")?;
            }
            other => return Err(format!("Unknown option for read: {other}\n\n{USAGE}")),
        }
        index += 1;
    }

    let client = connect().await?;
    let _ = terminal_info(&client, id)?;
    let answer = ask(
        &client,
        json!({
            "type": "terminal-read",
            "pane_id": id,
            "lines": u64::try_from(lines).unwrap_or(u64::MAX),
        }),
    )?;
    let text = answer.get("text").and_then(Value::as_str).unwrap_or("");
    print!("{text}");
    if !text.is_empty() && !text.ends_with('\n') {
        println!();
    }
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// `send`: writes the text to the terminal and presses Enter.
async fn send_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`send` needs a terminal id.\n\n{USAGE}"))?;
    let mut text = args[1..].join(" ");
    if text.is_empty() {
        return Err(format!("`send` needs text to send.\n\n{USAGE}"));
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }

    let client = connect().await?;
    let _ = terminal_info(&client, id)?;
    ask(
        &client,
        json!({
            "type": "terminal-send",
            "pane_id": id,
            "text": text,
        }),
    )?;
    println!("sent {id}");
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// `wait`: returns when the terminal reports, ends, or the timeout runs out.
async fn wait_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`wait` needs a terminal id.\n\n{USAGE}"))?;
    let mut timeout = Duration::from_secs(30);
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--timeout" => {
                index += 1;
                let secs: u64 = flag_value(args, index, "--timeout")?
                    .parse()
                    .map_err(|_| "--timeout must be a whole number of seconds")?;
                timeout = Duration::from_secs(secs);
            }
            other => return Err(format!("Unknown option for wait: {other}\n\n{USAGE}")),
        }
        index += 1;
    }

    let client = connect().await?;
    let _ = terminal_info(&client, id)?;
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let answer = ask(
        &client,
        json!({
            "type": "terminal-wait",
            "pane_id": id,
            "timeout_ms": timeout_ms,
        }),
    )?;
    println!("{}", describe_wait(&answer, timeout));
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// What `wait` prints for a `terminal-wait` answer.
fn describe_wait(answer: &Value, timeout: Duration) -> String {
    let location = answer
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if answer.get("done").and_then(Value::as_bool) == Some(true) {
        if let Some(status) = answer.get("status").and_then(Value::as_str) {
            return format!("done\nstatus: {status}\naddress: {location}");
        }
        if let Some(code) = answer.get("exited").and_then(Value::as_i64) {
            return format!("ended\nexit: {code}\naddress: {location}");
        }
        return format!("gone\naddress: {location}");
    }
    format!(
        "still running after {}s\naddress: {location}",
        timeout.as_secs()
    )
}

/// `kill`: ends the terminal and its children.
async fn kill_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`kill` needs a terminal id.\n\n{USAGE}"))?;
    let client = connect().await?;
    let _ = terminal_info(&client, id)?;
    ask(&client, json!({ "type": "terminal-kill", "pane_id": id }))?;
    println!("killed {id}");
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// `report`: records what a program said about itself.
async fn report_verb(args: &[String]) -> Result<ExitCode, String> {
    let id = args
        .first()
        .ok_or_else(|| format!("`report` needs a terminal id.\n\n{USAGE}"))?;
    let raw = args.get(1).ok_or_else(|| {
        format!("`report` needs a status: working, done, blocked, error.\n\n{USAGE}")
    })?;
    let status = ReportedStatus::from_wire(raw).ok_or_else(|| {
        format!("Unknown status: {raw}\nStatuses: working, done, blocked, error.")
    })?;

    let client = connect().await?;
    let _ = terminal_info(&client, id)?;
    ask(
        &client,
        json!({
            "type": "terminal-report",
            "pane_id": id,
            "status": status.as_wire(),
        }),
    )?;
    println!("reported {id} {}", status.as_wire());
    flush_stdout()?;
    Ok(ExitCode::SUCCESS)
}

/// Flushes stdout so a caller reading a pipe sees the answer at once.
fn flush_stdout() -> Result<(), String> {
    std::io::stdout()
        .flush()
        .map_err(|error| format!("Cannot write to stdout: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion_core::runtime::RuntimeDaemon;

    /// A session no other test on this machine uses.
    fn unique_session(tag: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The words of a command line, as the CLI receives them.
    fn words(line: &[&str]) -> Vec<String> {
        line.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn the_program_and_its_arguments_reach_the_program() {
        let line = words(&["run", "--", "cmd.exe", "/C", "echo hi"]);
        let request = parse_spawn_request(&line[1..]).expect("the spawn parses");
        let SpawnTarget::Program { program, args } = request.target else {
            panic!("a program after `--` must stay a program");
        };
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, ["/C", "echo hi"]);
    }

    #[test]
    fn an_agent_kind_needs_no_program_arguments() {
        let line = words(&["run", "--agent", "claude", "--name", "reviewer"]);
        let request = parse_spawn_request(&line[1..]).expect("the spawn parses");
        let SpawnTarget::Agent(kind) = request.target else {
            panic!("`--agent` must stay an agent kind");
        };
        assert_eq!(kind, "claude");
        assert_eq!(request.name.as_deref(), Some("reviewer"));
    }

    #[test]
    fn a_program_with_no_arguments_is_still_a_program() {
        let line = words(&["run", "--", "cmd.exe"]);
        let request = parse_spawn_request(&line[1..]).expect("the spawn parses");
        let SpawnTarget::Program { program, args } = request.target else {
            panic!("a program after `--` must stay a program");
        };
        assert_eq!(program, "cmd.exe");
        assert!(args.is_empty(), "a program may take no arguments");
    }

    #[test]
    fn a_switch_a_posix_shell_rewrote_is_put_back() {
        let rewritten = words(&["C:/", "echo hi"]);
        assert_eq!(restore_posix_switches(&rewritten), ["/C", "echo hi"]);
        let real_path = words(&["C:/Users"]);
        assert_eq!(restore_posix_switches(&real_path), ["C:/Users"]);
    }

    #[test]
    fn the_agent_facing_document_names_every_verb() {
        for verb in [
            "spawn-agent",
            "send",
            "read",
            "wait",
            "kill",
            "list",
            "report",
        ] {
            let example = format!("orion-rt {verb}");
            assert!(
                SKILL.contains(&example),
                "the skill document must show `{example}`"
            );
        }
    }

    #[test]
    fn the_usage_lists_every_verb() {
        for verb in [
            "spawn-agent",
            "list",
            "attach",
            "read",
            "send",
            "wait",
            "kill",
            "report",
        ] {
            let line = format!("\n  {verb}");
            assert!(USAGE.contains(&line), "the usage must list `{verb}`");
        }
        assert!(
            USAGE.contains("`run` is `spawn-agent`"),
            "the usage must name the `run` alias"
        );
        assert!(
            USAGE.contains("`ls` is `list`"),
            "the usage must name the `ls` alias"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wait_that_times_out_prints_the_address() {
        let session = unique_session("wait");
        let daemon = RuntimeDaemon::start(&session)
            .await
            .expect("the daemon starts");
        let stream = RuntimeStream::connect(&session)
            .await
            .expect("the runtime connects");
        let client = RuntimeClient::new(Box::new(stream)).expect("the handshake completes");

        // A program that outlives the timeout, so the timeout is what ends the wait.
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec![
                    "/D".to_string(),
                    "/C".to_string(),
                    "ping -n 30 127.0.0.1 >nul".to_string(),
                ],
            )
        } else {
            ("/bin/sh", vec!["-c".to_string(), "sleep 30".to_string()])
        };
        let spawn = ask(
            &client,
            json!({
                "type": "terminal-spawn",
                "program": program,
                "args": args,
                "cwd": std::env::temp_dir().to_string_lossy(),
                "name": "slow",
                "parent_id": Value::Null,
                "outlive_parent": false,
            }),
        )
        .expect("the terminal starts");
        let id = spawn
            .get("terminal_id")
            .and_then(Value::as_str)
            .expect("a terminal id")
            .to_string();

        let timeout = Duration::from_millis(300);
        let answer = ask(
            &client,
            json!({
                "type": "terminal-wait",
                "pane_id": id,
                "timeout_ms": 300,
            }),
        )
        .expect("the wait answers");
        assert_eq!(
            answer.get("done").and_then(Value::as_bool),
            Some(false),
            "a running program is not done: {answer}"
        );

        let printed = describe_wait(&answer, timeout);
        let expected = address(&session, &id);
        assert!(
            printed.contains(&expected),
            "the timeout must print the address {expected}: {printed}"
        );

        ask(&client, json!({ "type": "terminal-kill", "pane_id": id }))
            .expect("the terminal is killed");
        daemon.shutdown().await;
    }
}
