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
extern crate chan_signal;

use std::env;
use std::u64;
use std::str::FromStr;
use std::process::exit;
use std::process::Command;
use std::process::Child;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use getopts::Options;

use chan_signal::Signal;
use chan::{Sender, Receiver};
use std::os::unix::io::{RawFd, IntoRawFd};

struct EinConfig {
    childhood: Duration,
    retries: u64,
    count: u64,
    bind_fds: Vec<RawFd>,
    prog: Command,
    //XXX:rpc_ask: Sender<String>,
    //XXX:rpc_reply: Receiver<Result<String, String>>,
}

enum OffspringState {
    Expectant,  // no process exist yet
    Infancy,    // just started, waiting for ACK
    Healthy,
    Sick,
    Notified,   // shutting down
    Dead,
}

struct Offspring {
    state: OffspringState,
    process: Option<Child>,
    birthday: Instant,    // specifies the generation
    attempts: u64,
}

// This is the main event loop
fn shepard(mut cfg: EinConfig, signal_rx: Receiver<Signal>) {

    // birth the initial set of offspring

    // create timer
    let timer = timer::Timer::new();
    // XXX: these signatures are bogus
    let (timer_tx, timer_rx): (Sender<u64>, Receiver<u64>) = chan::async();

    // infinite select() loop over timers, signals, rpc

    loop {
        chan_select! {
            timer_rx.recv() => println!("Timer tick'd"),
            signal_rx.recv() => {
                println!("Signal received!");
                break;
            }
        }
    }

    let mut children: Vec<Child> = vec![];
    for _ in 0..cfg.count {
        println!("Running!");
        children.push(cfg.prog.spawn().expect("error spawning"));
    }

    println!("Waiting for all children to die");
    for mut c in children {
        c.wait().unwrap();
    }
    println!("Done.");
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
        childhood: Duration::new(3, 0),
        retries: 3,
        bind_fds: vec![],
        prog: Command::new(""),
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
        TcpListener::bind(sa).unwrap()
    }).collect();

    let mut prog = Command::new(&program_and_args[0]);
    prog.args(&program_and_args[1..]);

    cfg.bind_fds = binds.into_iter().map(|b| {
        let orig_fd = b.into_raw_fd();
        // Duplicate, which also clears the CLOEXEC flag
        let fd = nix::unistd::dup(orig_fd).unwrap();
        println!("fd={} FD_CLOEXEC={}", fd, nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).unwrap());
        fd
    }).collect();

    prog.env("EINHORN_FD_COUNT", cfg.bind_fds.len().to_string());
    // This iterator destroys the TcpListeners
    for (i, fd) in cfg.bind_fds.iter().enumerate() {
        prog.env(format!("EINHORN_FD_{}", i), fd.to_string());
    }
    cfg.prog = prog;

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
