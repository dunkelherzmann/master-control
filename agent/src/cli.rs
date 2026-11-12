//! The command line, as a function, so one program can be the worker, the master and the window.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::{default_home, Agent, Config, DEFAULT_PORT, VERSION};

const USAGE: &str = "\
Master Control agent

  mc-agent serve [--port N] [--home DIR]   Serve the HTTP API and MCP on 127.0.0.1 (default port 47900)
         [--worker] [--worker-port N] [--name NAME]
                                           Also be a worker: available to one master across the network
                                           (default port 47901), announced under NAME (default: the host name)
  mc-agent pair CODE [MASTER] [--home DIR] Pair this worker with the master showing CODE. MASTER is its address;
                                           without it the master is looked for on the network
  mc-agent unpair [--home DIR]             Forget this worker's master (then restart the worker)
  mc-agent start [--name NAME] [--any-network]
                                           Install this program as a worker that starts with the desktop session and
                                           has admin rights (Windows: run from an elevated prompt). On Windows the
                                           master is let in from private networks; --any-network adds public ones
  mc-agent cleanup [--home DIR]            Remove all of it: the worker, its start-up entry, its firewall rule, and its
                                           folder with the pairing, token, logs and recorded sessions
  mc-agent mcp [--home DIR]                Serve MCP on stdin and stdout
  mc-agent token [--home DIR]              Print the token requests must carry
  mc-agent --version

The home folder holds the token, the audit log and jobs' output. Default: MC_HOME, or the system's place for an
app's data.";

/// Runs the command line: `arguments` are the words after the program's name.
pub fn run(arguments: Vec<String>) -> ExitCode {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().unwrap_or_default();
    #[cfg(any(windows, target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    if command == "observe-run" {
        let outcome = match arguments.next() {
            Some(directory) => crate::observe::tracer::run(std::path::Path::new(&directory)),
            None => Err(std::io::Error::other("observe-run needs the session's folder")),
        };
        return match outcome {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&error.to_string()),
        };
    }
    let mut home = default_home();
    let mut port = DEFAULT_PORT;
    let mut worker: Option<crate::worker::WorkerConfig> = None;
    let as_worker = |worker: &mut Option<crate::worker::WorkerConfig>| {
        worker
            .get_or_insert(crate::worker::WorkerConfig {
                // Every address of the machine, unless MC_WORKER_ADDRESS names one (127.0.0.1 keeps it to this machine,
                // for trying a worker out).
                address: std::env::var("MC_WORKER_ADDRESS").ok().and_then(|text| text.parse().ok()).unwrap_or(std::net::Ipv4Addr::UNSPECIFIED.into()),
                port: crate::worker::WORKER_PORT,
                name: None,
                announce_to: Vec::new(),
            })
            .clone()
    };
    let mut any_network = false;
    // What follows `pair`: the code (which may be written in two groups) and perhaps the master's address.
    let mut pair_words: Vec<String> = Vec::new();
    while let Some(option) = arguments.next() {
        if option == "--any-network" {
            any_network = true;
            continue;
        }
        if command == "pair" && !option.starts_with("--") {
            pair_words.push(option);
            continue;
        }
        if option == "--worker" {
            as_worker(&mut worker);
            continue;
        }
        let value = arguments.next();
        match (option.as_str(), value) {
            ("--home", Some(folder)) => home = PathBuf::from(folder),
            ("--port", Some(number)) => match number.parse() {
                Ok(number) => port = number,
                Err(_) => return fail(&format!("--port takes a number from 0 to 65535, not {number}")),
            },
            ("--worker-port", Some(number)) => match number.parse() {
                Ok(number) => worker = Some(crate::worker::WorkerConfig { port: number, ..as_worker(&mut worker) }),
                Err(_) => return fail(&format!("--worker-port takes a number from 0 to 65535, not {number}")),
            },
            ("--name", Some(name)) => worker = Some(crate::worker::WorkerConfig { name: Some(name), ..as_worker(&mut worker) }),
            ("--home" | "--port" | "--worker-port" | "--name", None) => return fail(&format!("{option} needs a value")),
            _ => return fail(&format!("unknown option {option}\n\n{USAGE}")),
        }
    }
    let outcome = match command.as_str() {
        "serve" => crate::start(Config { home: home.clone(), port, worker }).map(|running| {
            eprintln!("mc-agent {VERSION} listening on 127.0.0.1:{}", running.port);
            if let Some(worker_port) = running.worker_port {
                eprintln!("worker: available to its master on port {worker_port}");
                match crate::worker::master_name(&home) {
                    Some(master) => eprintln!("worker: paired with {master}"),
                    None => eprintln!("worker: not paired yet; pair it with the code its master shows (mc-agent pair CODE)"),
                }
            }
            running.wait();
        }),
        "mcp" => Agent::new(home).and_then(|agent| crate::mcp::serve_stdio(&agent, std::io::stdin().lock(), std::io::stdout().lock())),
        "token" => Agent::new(home).map(|agent| println!("{}", agent.token())),
        "pair" => pair(&home, &pair_words),
        "unpair" => crate::worker::unpair(&home),
        "start" => crate::install::install(worker.as_ref().and_then(|worker| worker.name.as_deref()), any_network).map(|done| done.iter().for_each(|line| println!("{line}"))),
        "cleanup" => crate::install::uninstall().map(|mut done| {
            if std::fs::remove_dir_all(&home).is_ok() {
                done.retain(|line| line != "nothing was installed");
                done.push(format!("removed {}", home.display()));
            }
            done.iter().for_each(|line| println!("{line}"));
        }),
        "--version" | "-V" => {
            println!("mc-agent {VERSION}");
            Ok(())
        }
        "" | "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => return fail(&format!("unknown command {other}\n\n{USAGE}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error.to_string()),
    }
}

fn pair(home: &std::path::Path, words: &[String]) -> std::io::Result<()> {
    // The address is the word with a dot or a colon in it; the rest is the code.
    let (address, code): (Vec<&String>, Vec<&String>) = words.iter().partition(|word| word.contains('.') || word.contains(':'));
    let code = code.iter().map(|word| word.as_str()).collect::<Vec<_>>().join(" ");
    if code.is_empty() {
        return Err(std::io::Error::other("give the code the master shows: mc-agent pair CODE [MASTER]"));
    }
    let master = match address.first() {
        Some(address) => address.to_string(),
        None => {
            let listen = std::net::SocketAddr::from(([0, 0, 0, 0], crate::worker::ANNOUNCE_PORT));
            match crate::worker::find_master(listen, std::time::Duration::from_secs(8))? {
                Some((address, name)) => {
                    eprintln!("found the master {name} at {address}");
                    address
                }
                None => return Err(std::io::Error::other("no master is showing a pairing code on this network; give its address: mc-agent pair CODE ADDRESS")),
            }
        }
    };
    let name = crate::worker::pair_with_master(home, &master, &code)?;
    println!("paired with {name}");
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("mc-agent: {message}");
    ExitCode::from(2)
}
