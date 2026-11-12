//! Master Control as one program. Started plainly it opens the window; under the name the worker or the master is
//! installed by (`mc-agent`, `mctl`), or with `agent` or `master` as its first word, it is that command line.

// Where the window isn't built yet, the screens and what draws them are compiled and tested but not used.
#![cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]

#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

mod canvas;
mod ui;
#[cfg(target_os = "linux")]
#[path = "window_x11.rs"]
mod window;
#[cfg(windows)]
#[path = "window_win.rs"]
mod window;
#[cfg(target_os = "macos")]
#[path = "window_mac.rs"]
mod window;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use ui::{Role, Showing, Status, Work};

struct Real;

struct Code(mctl::pairing::Window);

impl Showing for Code {
    fn code(&self) -> String {
        self.0.code()
    }
    fn paired(&self) -> Vec<(String, String)> {
        self.0.paired().into_iter().map(|machine| (machine.name, machine.address)).collect()
    }
    fn seconds_left(&self) -> u64 {
        self.0.seconds_left()
    }
}

fn lines(outcome: std::io::Result<Vec<String>>) -> Vec<String> {
    outcome.unwrap_or_else(|error| vec![format!("It didn't work: {error}")])
}

impl Work for Real {
    fn start(&self, role: Role, public_networks: bool) -> Vec<String> {
        match role {
            Role::Master => lines(mctl::role::start(public_networks)),
            Role::Worker => lines(mc_agent::install::install(None, public_networks)),
        }
    }

    fn cleanup(&self, role: Role) -> Vec<String> {
        match role {
            Role::Master => lines(mctl::Master::open(mctl::default_home()).and_then(mctl::role::cleanup)),
            Role::Worker => lines(mc_agent::install::uninstall().map(|mut done| {
                let home = mc_agent::default_home();
                if std::fs::remove_dir_all(&home).is_ok() {
                    done.retain(|line| line != "nothing was installed");
                    done.push(format!("removed {}", home.display()));
                }
                done
            })),
        }
    }

    fn show_code(&self) -> Result<Box<dyn Showing>, String> {
        let master = mctl::Master::open(mctl::default_home()).map_err(|error| format!("The master's folder can't be used: {error}"))?;
        let window = mctl::pairing::open(master.home().to_path_buf(), master.own_name(), std::net::Ipv4Addr::UNSPECIFIED.into(), mctl::PAIR_PORT, Vec::new(), 10)
            .map_err(|error| format!("The pairing window couldn't be opened: {error}"))?;
        Ok(Box::new(Code(window)))
    }

    fn pair_with(&self, code: &str) -> Vec<String> {
        let listen = std::net::SocketAddr::from(([0, 0, 0, 0], mc_agent::worker::ANNOUNCE_PORT));
        let found = match mc_agent::worker::find_master(listen, Duration::from_secs(8)) {
            Ok(Some(found)) => found,
            Ok(None) => return vec!["No master is showing a pairing code on this network.".into(), "Open Pair on the master, then try again.".into()],
            Err(error) => return vec![format!("Couldn't listen for the master: {error}")],
        };
        match mc_agent::worker::pair_with_master(&mc_agent::default_home(), &found.0, code) {
            Ok(name) => vec![format!("Paired with {name} at {}.", found.0)],
            Err(error) => vec![format!("Not paired: {error}")],
        }
    }

    fn open(&self, address: &str) {
        let (program, before): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("open", &[])
        } else if cfg!(windows) {
            ("rundll32", &["url.dll,FileProtocolHandler"])
        } else {
            ("xdg-open", &[])
        };
        let quiet = std::process::Stdio::null;
        let _ = std::process::Command::new(program).args(before).arg(address).stdin(quiet()).stdout(quiet()).stderr(quiet()).spawn();
    }

    fn busy(&self) -> Vec<String> {
        // Calls the master is making, by the machine they go to. All of them pass through this machine, so its own
        // light flickers with any of them.
        let mut busy: Vec<String> = mctl::active_machines(&mctl::default_home(), 600).into_iter().filter(|machine| !matches!(machine.to_lowercase().as_str(), "master" | "self" | "local")).collect();
        let through_here = !mctl::active_machines(&mctl::default_home(), 600).is_empty();
        // On a worker: requests that came in from its master, by the end of its own record of them.
        let asked_here = std::fs::read(mc_agent::default_home().join("audit.log")).ok().is_some_and(|bytes| {
            let tail = &bytes[bytes.len().saturating_sub(600)..];
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
            // The record's lines start with the time: {"time_ms":1794420000000,...
            String::from_utf8_lossy(tail).lines().last().and_then(|line| line.split_once("\"time_ms\":")).and_then(|(_, rest)| rest.split(|character: char| !character.is_ascii_digit()).next()?.parse::<u64>().ok()).is_some_and(|time| now.saturating_sub(time) <= 600)
        });
        if through_here || asked_here {
            busy.push("This machine".into());
        }
        busy
    }

    fn live(&self) -> ui::Live {
        use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
        // Whether something answers at an address, asked quickly: a machine that is off must not hold the others up.
        let answers = |address: &str| address.to_socket_addrs().ok().and_then(|mut found| found.next()).is_some_and(|address: SocketAddr| TcpStream::connect_timeout(&address, Duration::from_millis(350)).is_ok());
        let mut lines = Vec::new();
        let master_home = mctl::default_home();
        let master_installed = mctl::role::installed_program().exists();
        let agents: Vec<String> = mctl::agents::found(&mctl::agents::Places::here()).into_iter().filter(|agent| agent.registered).map(|agent| agent.name.to_string()).collect();
        let workers = if master_home.exists() { mctl::Master::open(master_home.clone()).map(|master| master.machines()).unwrap_or_default() } else { Vec::new() };
        let worker_installed = mc_agent::install::installed_program().exists();
        let running = worker_installed && answers(&format!("127.0.0.1:{}", mc_agent::worker::WORKER_PORT));
        let paired = mc_agent::worker::master_name(&mc_agent::default_home());
        // What this machine is. A master gives the agents on it everything a worker would give, and its workers
        // besides: it is the master, and nothing is said about it not being a worker.
        if master_installed {
            lines.push(ui::Line {
                well: Some(!agents.is_empty()),
                name: "This machine".into(),
                says: if agents.is_empty() { "is the master; no AI agent has it yet (press Start)".into() } else { format!("is the master, for {}", agents.join(", ")) },
            });
        }
        if worker_installed {
            lines.push(ui::Line {
                well: Some(running),
                name: "This machine".into(),
                says: match (running, &paired) {
                    (false, _) => "is a worker, but isn't running".into(),
                    (true, Some(master)) => format!("is a worker of {master}"),
                    (true, None) => "is a worker, not paired yet".into(),
                },
            });
        }
        if !master_installed && !worker_installed {
            lines.push(ui::Line { well: None, name: "This machine".into(), says: "isn't set up yet: choose Master or Worker".into() });
        }
        for worker in &workers {
            let up = answers(&worker.address);
            lines.push(ui::Line { well: Some(up), name: worker.name.clone(), says: format!("{} at {}", if up { "worker, answers" } else { "worker, doesn't answer" }, worker.address) });
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
        let ago = |then: u64| match now.saturating_sub(then) / 1000 {
            seconds @ 0..=119 => format!("{seconds} s ago"),
            seconds @ 120..=7199 => format!("{} min ago", seconds / 60),
            seconds => format!("{} h ago", seconds / 3600),
        };
        let calls = mctl::recent_activity(&master_home, 6)
            .into_iter()
            .map(|call| format!("{}   {}   {}   {}   {} ms", ago(call.time_ms), call.machine, call.tool, if call.ok { "ok".to_string() } else { format!("refused ({})", call.code) }, call.took_ms))
            .collect();
        ui::Live { lines, calls, busy: Vec::new() }
    }

    fn identity(&self) -> (String, String) {
        let identity = mc_agent::identity::this_machine();
        (identity.hostname.clone(), format!("{} · {}", identity.os_release, identity.mac))
    }

    fn status(&self, role: Role) -> Status {
        match role {
            Role::Master => Status {
                installed: mctl::role::installed_program().exists(),
                paired_with: mctl::Master::open(mctl::default_home()).map(|master| master.machines().into_iter().map(|machine| machine.name).collect()).unwrap_or_default(),
                agents: mctl::agents::found(&mctl::agents::Places::here()).into_iter().map(|agent| (agent.name.to_string(), agent.registered)).collect(),
            },
            Role::Worker => Status {
                installed: mc_agent::install::installed_program().exists(),
                paired_with: mc_agent::worker::master_name(&mc_agent::default_home()).into_iter().collect(),
                agents: Vec::new(),
            },
        }
    }
}

fn main() -> ExitCode {
    let mut arguments: Vec<String> = std::env::args().collect();
    let called_as = std::path::Path::new(&arguments.remove(0)).file_stem().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
    if called_as.starts_with("mc-agent") {
        return mc_agent::cli::run(arguments);
    }
    if called_as.starts_with("mctl") {
        return mctl::cli::run(arguments);
    }
    match arguments.first().map(String::as_str) {
        Some("agent") => return mc_agent::cli::run(arguments.split_off(1)),
        Some("master") => return mctl::cli::run(arguments.split_off(1)),
        // The recorder of an observation session, when the program that asked for it is this one.
        Some("observe-run") => return mc_agent::cli::run(arguments),
        // A picture of one screen as the window draws it, for the design folder and for checking a change by eye.
        Some("picture") => {
            let (Some(screen), Some(file)) = (arguments.get(1), arguments.get(2)) else {
                eprintln!("master-control picture choose|master|worker|about FILE.png");
                return ExitCode::from(2);
            };
            let mut app = ui::App::new(Arc::new(Real), 2.0);
            app.set_identity("this-machine.local", "macOS 27.2 · 00:11:22:33:44:55");
            // A stand-in for what is going on, unless the real thing is asked for.
            if std::env::var_os("MC_PICTURE_LIVE").is_some() {
                app.look_now();
            } else {
                let line = |well, name: &str, says: &str| ui::Line { well, name: name.into(), says: says.into() };
                app.set_live(ui::Live {
                    lines: vec![
                        line(Some(true), "This machine", "is the master, for Claude Code, Codex"),
                        line(Some(true), "linux", "worker, answers at 192.0.2.41:47901"),
                        line(Some(false), "windows", "worker, doesn't answer at 192.0.2.31:47901"),
                    ],
                    calls: vec!["12 s ago   linux   screenshot   ok   212 ms".into(), "9 s ago   linux   find   ok   48 ms".into(), "2 s ago   windows   status   refused (unreachable)   350 ms".into()],
                    busy: Vec::new(),
                });
            }
            match screen.as_str() {
                "master" => app.act(ui::Action::Choose(ui::Role::Master)),
                "worker" => app.act(ui::Action::Choose(ui::Role::Worker)),
                "about" => app.act(ui::Action::About),
                _ => {}
            }
            let (width, height) = app.size();
            let mut canvas = canvas::Canvas::new(width, height);
            app.draw(&mut canvas);
            let bytes: Vec<u8> = canvas.pixels.iter().flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]).collect();
            let written = std::fs::File::create(file).map_err(|error| error.to_string()).and_then(|file| {
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().and_then(|mut writer| writer.write_image_data(&bytes)).map_err(|error| error.to_string())
            });
            return match written {
                Ok(()) => ExitCode::SUCCESS,
                Err(problem) => {
                    eprintln!("master-control: the picture couldn't be written: {problem}");
                    ExitCode::from(1)
                }
            };
        }
        Some("--version") => {
            println!("master-control {}", mc_core::VERSION);
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("master-control: {other} isn't something it does. Start it without words for the window, or with `agent` or `master` for those command lines.");
            return ExitCode::from(2);
        }
        None => {}
    }
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        match window::run(Arc::new(Real)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(problem) => {
                eprintln!("master-control: {problem}");
                ExitCode::from(1)
            }
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = ui::App::new(Arc::new(Real), 1.0).size();
        eprintln!("master-control: the window isn't built for this system yet. Use `master-control master …` and `master-control agent …`.");
        ExitCode::from(1)
    }
}
