/*
 *  einhyrningsins: graceful restarts for socket-based daemons
 *  Copyright (C) 2016  Bryan Newbold <bnewbold@robocracy.org>
 *
 *  This program is free software: you can redistribute it and/or modify
 *  it under the terms of the GNU General Public License as published by
 *  the Free Software Foundation, either version 3 of the License, or
 *  (at your option) any later version.
 *
 *  This program is distributed in the hope that it will be useful,
 *  but WITHOUT ANY WARRANTY; without even the implied warranty of
 *  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *  GNU General Public License for more details.
 *
 *  You should have received a copy of the GNU General Public License
 *  along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

#[macro_use]
extern crate chan;

extern crate getopts;
extern crate log;
extern crate env_logger;
extern crate nix;
extern crate timer;
extern crate time;
extern crate chan_signal;
extern crate uuid;

use std::env;
use std::u64;
use std::str::FromStr;
use std::process::exit;
use std::process::Command;
use std::process::Child;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::ToSocketAddrs;
use std::time::Instant;
use time::Duration;
use std::collections::HashMap;
use getopts::Options;
use uuid::Uuid;

use chan_signal::Signal;
use chan::{Sender, Receiver};
use std::os::unix::io::{RawFd, IntoRawFd};

struct EinConfig {
    childhood: Duration,
    retries: u64,
    count: u64,
    bind_fds: Vec<RawFd>,
    cmd: Command,
    //TODO:rpc_ask: Sender<String>,
    //TODO:rpc_reply: Receiver<Result<String, String>>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum OffspringState {
    Expectant,  // no process exist yet
    Infancy,    // just started, waiting for ACK
    Healthy,
    Notified,   // shutting down gracefully
    Dead,
}

struct Offspring {
    id: Uuid,
    state: OffspringState,
    process: Option<Child>,
    birthday: Instant,    // specifies the generation
    attempts: u64,
}

impl Offspring {

    pub fn new() -> Offspring {
        Offspring {
            id: Uuid::new_v4(),
            state: OffspringState::Expectant,
            process: None,
            birthday: Instant::now(),
            attempts: 0,
        }
    }

    pub fn spawn(&mut self, cfg: &mut EinConfig) -> Result<(), String> {
        if self.is_active() {
            return Err(format!("Can't spawn from state: {:?}", self.state));
        }
        self.process = Some(cfg.cmd.spawn().expect("error spawning"));
        self.birthday = Instant::now();
        self.attempts = 0;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        match self.state {
            OffspringState::Expectant => false,
            OffspringState::Infancy => true,
            OffspringState::Healthy => true,
            OffspringState::Notified => true,
            OffspringState::Dead => false,
        }
    }

    pub fn signal(&mut self, sig: Signal) {
        if !self.is_active() {
            return;
        }
        let nix_sig = match sig {
            Signal::HUP => nix::sys::signal::Signal::SIGHUP,
            Signal::INT => nix::sys::signal::Signal::SIGINT,
            _ => { println!("Unexpected signal: {:?}", sig); return; },
        };
        match self.process {
            Some(ref p) => { nix::sys::signal::kill(p.id() as i32, nix_sig).unwrap(); },
            None => (),
        }
        // TODO: abs probably isn't the best way to i32 -> u32 here
        //nix::sys::signal::kill(num::abs(self.process.unwrap().id()), nix_sig);
        //let pid = self.process.unwrap().id();
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum TimerAction {
    CheckAlive(Uuid),
}

// This is the main event loop
fn shepard(mut cfg: EinConfig, signal_rx: Receiver<Signal>) {

    //// create timer
    let timer = timer::Timer::new();
    let (timer_tx, timer_rx): (Sender<TimerAction>, Receiver<TimerAction>) = chan::async();

    //// birth the initial set of offspring
    let mut brood: HashMap<Uuid, Offspring> = HashMap::new();
    for _ in 0..cfg.count {
        println!("Running!");
        let mut o = Offspring::new();
        o.spawn(&mut cfg).unwrap();
        let t_tx = timer_tx.clone();
        let o_id = o.id.clone();
        timer.schedule_with_delay(cfg.childhood, move || {
            t_tx.send(TimerAction::CheckAlive(o_id));
        });
        brood.insert(o.id, o);
    }

    //// infinite select() loop over timers, signals, rpc
    loop {
        chan_select! {
            timer_rx.recv() => { println!("Timer tick'd"); "TIMER" },
            signal_rx.recv() -> sig => match sig.expect("Error with signal handler") {
                // XXX: Signal::HUP => brood.iter().for_each(|o| o.signal(sig)),
                Signal::INT | Signal::TERM=> {
                    println!("Notifying children...");
                    // XXX: brood.iter().for_each(|o| o.signal(sig));
                    break;
                },
                _ => "Other"
            }
        }
    }

/* XXX:
    println!("Waiting for all children to die");
    for mut o in brood.values() {
        match o.process {
            Some(ref mut p) => { p.wait().unwrap(); () },
            None => (),
        }
    }
    println!("Done.");
*/
}

fn print_usage(opts: Options) {
    let brief = "usage:\teinhyrningsins [options] program";
    println!("");
    print!("{}", opts.usage(&brief));
}

fn main() {

    let args: Vec<String> = env::args().collect();

    let mut opts = Options::new();
    opts.parsing_style(getopts::ParsingStyle::StopAtFirstFree);
    opts.optflag("h", "help", "print this help menu");
    opts.optflag("v", "verbose", "more debugging messages");
    opts.optopt("n", "number", "how many program copies to spawn", "COUNT");
    opts.optmulti("b", "bind", "socket(s) to bind to", "ADDR");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { println!("{}", f.to_string()); print_usage(opts); exit(-1); }
    };          

    if matches.opt_present("h") {
        print_usage(opts);
        return;
    }

    //// Parse Configuration
    let mut cfg = EinConfig{
        count: 1,
        childhood: Duration::seconds(3),
        retries: 3,
        bind_fds: vec![],
        cmd: Command::new(""),
    };

    cfg.count = match matches.opt_str("number") {
        Some(n) => u64::from_str(&n).expect("number arg should be an integer"),
        None => 1   // XXX: duplicate default
    };

    //// Bind Sockets
    let sock_addrs: Vec<SocketAddr> = matches.opt_strs("bind").iter().map(|b| {
        b.to_socket_addrs().unwrap().next().unwrap()
    }).collect();

    let program_and_args = if !matches.free.is_empty() {
        matches.free
    } else {
        print_usage(opts);
        exit(-1);
    };

    let mut builder = env_logger::LogBuilder::new();
    builder.parse("INFO");
    if env::var("RUST_LOG").is_ok() {
        builder.parse(&env::var("RUST_LOG").unwrap());
    }
    builder.init().unwrap();

    let binds: Vec<TcpListener> = sock_addrs.iter().map(|sa| {
        // XXX: SO_REUSE here
        TcpListener::bind(sa).unwrap()
    }).collect();

    let mut cmd = Command::new(&program_and_args[0]);
    cmd.args(&program_and_args[1..]);

    // TODO: check that program exists and is executable

    cfg.bind_fds = binds.into_iter().map(|b| {
        let orig_fd = b.into_raw_fd();
        // Duplicate, which also clears the CLOEXEC flag
        let fd = nix::unistd::dup(orig_fd).unwrap();
        println!("fd={} FD_CLOEXEC={}", fd, nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).unwrap());
        fd
    }).collect();

    cmd.env("EINHORN_FD_COUNT", cfg.bind_fds.len().to_string());
    // This iterator destroys the TcpListeners
    for (i, fd) in cfg.bind_fds.iter().enumerate() {
        cmd.env(format!("EINHORN_FD_{}", i), fd.to_string());
    }
    cfg.cmd = cmd;

    //// Listen for signals (before any fork())
    println!("Registering signal handlers...");
    let signal_rx = chan_signal::notify(&[Signal::INT,
                                          Signal::TERM,
                                          //Signal::CHLD, // XXX: PR has been submitted
                                          Signal::USR2,
                                          Signal::HUP]);

    shepard(cfg, signal_rx);
    exit(0);
}
