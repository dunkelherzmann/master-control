//! The command line, as a function, so one program can be the worker, the master and the window.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use crate::{default_home, scan, Master, ANNOUNCE_PORT};
use serde_json::Value;

const USAGE: &str = "\
Master Control, the master's side

  mctl start [--any-network]       Install what the AI agents on this machine need to use Master Control: Claude Code,
                                   the Claude app, Codex, Antigravity, Gemini CLI, Cursor, Windsurf and VS Code get
                                   it added to their MCP servers, wherever they are found
  mctl agents                      The AI agents found on this machine, and whether each has Master Control
  mctl guide                       Print the guide for AI agents: how to connect to Master Control and use it
  mctl cleanup                     Remove all of it: workers are told to forget this master, the agents lose their
                                   access, the master's folder and program are deleted
  mctl scan [SECONDS]              Listen for workers announcing themselves on the network (default 5 seconds)
  mctl pair [MINUTES]              Show a pairing code and take in the workers that are given it (default 10 minutes;
                                   press Enter to stop sooner)
  mctl machines                    This machine and the workers it is paired with
  mctl call MACHINE TOOL [JSON]    Run a tool on a machine (master is this one), with its arguments as a JSON object
  mctl unpair MACHINE              Forget a worker
  mctl mcp                         Serve MCP on stdin and stdout: this machine's and every worker's tools for an AI agent

Workers are remembered in MC_MASTER_HOME, or ~/.master-control/master.";

/// Runs the command line: `arguments` are the words after the program's name.
pub fn run(arguments: Vec<String>) -> ExitCode {
    // An observation session on the master's own machine is recorded by this same program, started again by the
    // agent inside it.
    if arguments.first().is_some_and(|word| word == "observe-run") {
        return mc_agent::cli::run(arguments);
    }
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let master = match Master::open(default_home()) {
        Ok(master) => master,
        Err(error) => return fail(&format!("the master's folder can't be used: {error}")),
    };
    let outcome: Result<(), String> = match words.as_slice() {
        ["scan"] | ["scan", _] => {
            let seconds = words.get(1).map_or(Ok(5), |text| text.parse::<u64>()).map_err(|_| "scan takes a number of seconds".to_string());
            seconds.and_then(|seconds| {
                let heard = scan(SocketAddr::from(([0, 0, 0, 0], ANNOUNCE_PORT)), Duration::from_secs(seconds)).map_err(|error| format!("can't listen for workers: {error}"))?;
                let _ = master.remember_addresses(&heard);
                if heard.is_empty() {
                    println!("no workers heard; is one running with --worker on this network?");
                }
                for worker in heard {
                    let mine = master.machines().iter().any(|machine| machine.id == worker.id);
                    let state = if mine { "paired with this master" } else if worker.paired { "paired with another master" } else { "not paired" };
                    println!("{}\t{}\t{state}\tversion {}", worker.name, worker.address, worker.version);
                }
                Ok(())
            })
        }
        ["pair"] | ["pair", _] => words.get(1).map_or(Ok(10), |text| text.parse::<u64>()).map_err(|_| "pair takes a number of minutes".to_string()).and_then(|minutes| pair(&master, minutes)),
        ["machines"] => {
            println!("{}\tthis machine\tthe master itself", master.own_name());
            for machine in master.machines() {
                let state = match master.call(&machine, "status", &Value::Null) {
                    Ok(status) => format!("{} {}, version {}", status["os"].as_str().unwrap_or("?"), status["arch"].as_str().unwrap_or("?"), status["version"].as_str().unwrap_or("?")),
                    Err(failure) => format!("not reachable ({})", failure.message),
                };
                println!("{}\t{}\t{state}", machine.name, machine.address);
            }
            Ok(())
        }
        ["call", machine, tool] | ["call", machine, tool, _] => {
            let args = words.get(3).map_or(Ok(Value::Null), |text| serde_json::from_str(text)).map_err(|error| format!("the arguments aren't JSON: {error}"));
            args.and_then(|args| {
                let result = master.run(machine, tool, &args).map_err(|failure| failure.to_string())?;
                println!("{result}");
                Ok(())
            })
        }
        ["guide"] => {
            print!("{}", crate::agents::GUIDE);
            Ok(())
        }
        ["agents"] => {
            let found = crate::agents::found(&crate::agents::Places::here());
            if found.is_empty() {
                println!("no AI agent known to this version was found on this machine");
            }
            for agent in found {
                println!("{}\t{}\t{}", agent.name, if agent.registered { "has master-control" } else { "doesn't have master-control" }, agent.how);
            }
            println!("any other agent that speaks MCP: add a server that runs {} with the one argument mcp", crate::role::installed_program().display());
            Ok(())
        }
        ["start"] | ["start", "--any-network"] => crate::role::start(words.len() == 2).map(|done| done.iter().for_each(|line| println!("{line}"))).map_err(|error| error.to_string()),
        ["cleanup"] => return match crate::role::cleanup(master) {
            Ok(done) => {
                done.iter().for_each(|line| println!("{line}"));
                ExitCode::SUCCESS
            }
            Err(error) => fail(&error.to_string()),
        },
        ["unpair", machine] => master.unpair(machine).map_err(|failure| failure.to_string()),
        ["mcp"] => crate::mcp::serve(&master, std::io::stdin().lock(), std::io::stdout().lock()).map_err(|error| error.to_string()),
        [] | ["help"] | ["--help"] | ["-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["--version"] => {
            println!("mctl {}", mc_core::VERSION);
            Ok(())
        }
        _ => Err(format!("that isn't a command\n\n{USAGE}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(&message),
    }
}

/// Shows the code, and names each worker as it pairs, until Enter is pressed or the time is up.
fn pair(master: &Master, minutes: u64) -> Result<(), String> {
    let window = crate::pairing::open(master.home().to_path_buf(), master.own_name(), std::net::Ipv4Addr::UNSPECIFIED.into(), crate::PAIR_PORT, Vec::new(), minutes)
        .map_err(|error| format!("the pairing window couldn't be opened: {error}"))?;
    let mut shown = String::new();
    let mut named = 0;
    let (sender, enter) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read_line(&mut String::new());
        let _ = sender.send(());
    });
    while !window.expired() && enter.try_recv().is_err() {
        if window.code() != shown {
            shown = window.code();
            println!("pairing code: {shown}   (on each worker: mc-agent pair {shown})");
        }
        for machine in window.paired().iter().skip(named) {
            println!("paired with {} at {}", machine.name, machine.address);
            named += 1;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("{} worker(s) paired", window.paired().len());
    window.close();
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("mctl: {message}");
    ExitCode::from(2)
}
