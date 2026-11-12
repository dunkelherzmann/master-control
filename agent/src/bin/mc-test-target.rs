//! A program for the observation tests to watch: it does exactly what its arguments say, in order, so a test knows
//! what the tracer should have seen. Not part of the shipped agent.
//!
//!   write=PATH|TEXT  read=PATH  delete=PATH  rename=FROM|TO  mkdir=PATH  connect=HOST:PORT  listen=PATH_FOR_PORT
//!   print=TEXT  complain=TEXT  sleep=MILLISECONDS  thread-write=PATH|TEXT  spawn (run the rest as a child)
//!   segv  abort  exit=CODE

use std::io::{Read, Write};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    for (index, argument) in arguments.iter().enumerate() {
        let (action, value) = argument.split_once('=').unwrap_or((argument.as_str(), ""));
        // A bar, not a colon: Windows paths have colons.
        let pair = || value.split_once('|').unwrap_or((value, ""));
        match action {
            "write" => std::fs::write(pair().0, pair().1).expect("write"),
            "read" => drop(std::fs::read(value).expect("read")),
            "delete" => std::fs::remove_file(value).expect("delete"),
            "rename" => std::fs::rename(pair().0, pair().1).expect("rename"),
            "mkdir" => std::fs::create_dir(value).expect("mkdir"),
            "connect" => {
                let mut stream = std::net::TcpStream::connect(value).expect("connect");
                stream.write_all(b"hello from the target").expect("send");
                let mut answer = [0u8; 64];
                let _ = stream.read(&mut answer);
            }
            "print" => println!("{value}"),
            "complain" => eprintln!("{value}"),
            "sleep" => std::thread::sleep(std::time::Duration::from_millis(value.parse().expect("milliseconds"))),
            "thread-write" => {
                let (path, text) = (pair().0.to_string(), pair().1.to_string());
                std::thread::spawn(move || std::fs::write(path, text).expect("write")).join().expect("thread");
            }
            "spawn" => {
                let status = std::process::Command::new(std::env::current_exe().expect("own path")).args(&arguments[index + 1..]).status().expect("spawn");
                std::process::exit(status.code().unwrap_or(1));
            }
            // A write to memory that isn't there: the classic crash.
            "segv" => unsafe { std::ptr::without_provenance_mut::<u8>(8).write_volatile(1) },
            "abort" => std::process::abort(),
            "exit" => std::process::exit(value.parse().expect("exit code")),
            other => panic!("unknown action {other}"),
        }
    }
}
