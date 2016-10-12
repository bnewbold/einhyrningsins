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
extern crate json;

extern crate getopts;
extern crate log;
extern crate env_logger;
extern crate nix;
extern crate timer;
extern crate time;
extern crate chan_signal;
extern crate url;

use std::io::prelude::*;
use std::io::{BufReader, BufWriter};
use std::env;
use std::fs;
use std::u64;
use std::str::FromStr;
use std::path::Path;
use std::process::exit;
use std::process::Command;
use std::process::Child;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::ToSocketAddrs;
use std::os::unix::net::{UnixStream, UnixListener};
use std::thread;
use time::Duration;
use std::collections::HashMap;
use getopts::Options;

use chan_signal::Signal;
use chan::{Sender, Receiver};
use std::os::unix::io::{RawFd, IntoRawFd};

// TODO: split this into read-only config and mutable state
struct EinConfig {
    childhood: Duration,
    graceperiod: Duration,
    manual_ack: bool,
    retries: u64,
    count: u64,
    bind_fds: Vec<RawFd>,
    cmd: Command,
    ipv4_only: bool,
    ipv6_only: bool,
    ctrl_req_rx: Receiver<CtrlRequest>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum OffspringState {
    Infancy,    // just started, waiting for ACK
    Healthy,
    Notified,   // shutting down gracefully
    Dead,
}

struct Offspring {
    state: OffspringState,
    process: Child,
    attempts: u64,
    timer_guard: Option<timer::Guard>,
    replaces: Option<u32>,
}

impl Offspring {

    pub fn spawn(cfg: &mut EinConfig, timer: &mut timer::Timer, t_tx: Sender<TimerAction>) -> Result<Offspring, String> {
        let mut o = Offspring {
            state: OffspringState::Infancy,
            process: cfg.cmd.spawn().expect("error spawning"),
            attempts: 0,
            timer_guard: None,
            replaces: None,
        };
        let pid = o.process.id();
        o.timer_guard = Some(timer.schedule_with_delay(cfg.childhood, move || {
            t_tx.send(TimerAction::CheckAlive(pid));
        }));
        Ok(o)
    }

    pub fn respawn(&mut self, cfg: &mut EinConfig, timer: &mut timer::Timer, t_tx: Sender<TimerAction>) -> Result<Offspring, String> {
        let mut successor = try!(Offspring::spawn(cfg, timer, t_tx));
        successor.replaces = Some(self.process.id());
        Ok(successor)
    }

    pub fn is_active(&self) -> bool {
        match self.state {
            OffspringState::Infancy => true,
            OffspringState::Healthy => true,
            OffspringState::Notified => true,
            OffspringState::Dead => false,
        }
    }

    pub fn kill(&mut self) {
        if !self.is_active() { return; }
        self.signal(Signal::KILL);
        self.state = OffspringState::Dead;
    }

    pub fn terminate(&mut self, cfg: &mut EinConfig, timer: &mut timer::Timer, t_tx: Sender<TimerAction>) {
        if !self.is_active() { return; }
        self.signal(Signal::TERM);
        self.state = OffspringState::Notified;
        let pid = self.process.id();
        self.timer_guard = Some(timer.schedule_with_delay(cfg.graceperiod, move || {
            t_tx.send(TimerAction::CheckTerminated(pid));
        }));
    }

    pub fn shutdown(&mut self, cfg: &mut EinConfig, timer: &mut timer::Timer, t_tx: Sender<TimerAction>) {
        if !self.is_active() { return; }
        self.signal(Signal::USR2);
        self.state = OffspringState::Notified;
        let pid = self.process.id();
        self.timer_guard = Some(timer.schedule_with_delay(cfg.graceperiod , move || {
            t_tx.send(TimerAction::CheckShutdown(pid));
        }));
    }

    pub fn signal(&mut self, sig: Signal) {
        if self.state == OffspringState::Dead {
            return;
        }
        let nix_sig = match sig {
            Signal::HUP   => nix::sys::signal::Signal::SIGHUP,
            Signal::INT   => nix::sys::signal::Signal::SIGINT,
            Signal::TERM  => nix::sys::signal::Signal::SIGTERM,
            Signal::KILL  => nix::sys::signal::Signal::SIGKILL,
            Signal::USR2  => nix::sys::signal::Signal::SIGUSR2,
            _ => { println!("Unexpected signal: {:?}", sig); return; },
        };
        nix::sys::signal::kill(self.process.id() as i32, nix_sig).unwrap();
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum TimerAction {
    CheckAlive(u32),
    CheckTerminated(u32),
    CheckShutdown(u32),
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum CtrlAction {
    Increment,
    Decrement,
    ManualAck(u32),
    SigAll(Signal),
    ShutdownAll,
    UpgradeAll,
    Status,
}

#[derive(Clone, Debug, PartialEq)]
struct CtrlRequest {
    action: CtrlAction,
    tx: Sender<String>,
}

// This is the main event loop
fn shepard(mut cfg: EinConfig, signal_rx: Receiver<Signal>) {

    //// create timer
    let mut timer = timer::Timer::new();
    let (timer_tx, timer_rx): (Sender<TimerAction>, Receiver<TimerAction>) = chan::async();

    //// birth the initial set of offspring
    let mut brood: HashMap<u32, Offspring> = HashMap::new();
    for _ in 0..cfg.count {
        let o = Offspring::spawn(&mut cfg, &mut timer, timer_tx.clone()).unwrap();
        let pid = o.process.id();
        brood.insert(pid, o);
        println!("Spawned: {}", pid); 
    }

    // Ugh, see: http://burntsushi.net/rustdoc/chan/macro.chan_select.html#failure-modes
    let ctrl_req_rx = cfg.ctrl_req_rx.clone();

    //// infinite select() loop over timers, signals
    let mut run = true;
    loop {
        chan_select! {
            timer_rx.recv() -> action => match action.expect("Error with timer thread") {
                TimerAction::CheckAlive(pid) => {
                    // Need to move 'o' out of HashMap here so we can mutate
                    // the map in other ways
                    if let Some(mut o) = brood.remove(&pid) {
                        if !cfg.manual_ack && o.state == OffspringState::Infancy {
                            if o.is_active() {
                                println!("{} found to be alive", pid);
                                o.state = OffspringState::Healthy;
                                if let Some(old_pid) = o.replaces {
                                    if let Some(old) = brood.get_mut(&old_pid) {
                                        old.shutdown(&mut cfg, &mut timer, timer_tx.clone());
                                    }
                                }
                            }
                        } else if cfg.manual_ack && o.state == OffspringState::Infancy {
                            println!("{} didn't check in", pid);
                            if o.attempts + 1 >= cfg.retries {
                                println!("Ran out of retries...");
                            } else {
                                let mut successor = o.respawn(&mut cfg, &mut timer, timer_tx.clone()).unwrap();
                                successor.attempts = o.attempts + 1;
                                brood.insert(successor.process.id(), successor);
                            }
                            o.terminate(&mut cfg, &mut timer, timer_tx.clone());
                        } else {
                            println!("Unexpected CheckAlive state! pid={} state={:?}", o.process.id(), o.state);
                        }
                        brood.insert(pid, o);
                    };
                },
                TimerAction::CheckShutdown(pid) => {
                    if let Some(o) = brood.get_mut(&pid) {
                        if o.is_active() {
                            o.terminate(&mut cfg, &mut timer, timer_tx.clone());
                        }
                    }
                },
                TimerAction::CheckTerminated(pid) => {
                    if let Some(o) = brood.get_mut(&pid) {
                        if o.is_active() {
                            o.kill();
                        }
                    }
                },
            },
            ctrl_req_rx.recv() -> maybe_req =>
                if let Some(req) = maybe_req { match req.action {
                    CtrlAction::Increment => {
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::Decrement => {
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::SigAll(sig) => {
                        for (_, o) in brood.iter_mut() {
                            o.signal(sig);
                        }
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::ShutdownAll => {
                        let mut pid_list = vec![];
                        for (pid, o) in brood.iter_mut() {
                            if o.is_active() {
                                o.shutdown(&mut cfg, &mut timer, timer_tx.clone());
                                pid_list.push(pid);
                            }
                        }
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::UpgradeAll => {
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::Status => {
                        req.tx.send(format!("UNIMPLEMENTED"));
                    },
                    CtrlAction::ManualAck(pid) => {
                        if let Some(o) = brood.get_mut(&pid) {
                            if o.is_active() {
                                o.state = OffspringState::Healthy;
                            }
                        }
                        req.tx.send(format!("Acknowledged!"));
                    },
                }
            },
            signal_rx.recv() -> sig => match sig.expect("Error with signal handler") {
                Signal::CHLD => {
                    loop {
                        let res = nix::sys::wait::waitpid(-1, Some(nix::sys::wait::WNOHANG));
                        match res {
                            Ok(nix::sys::wait::WaitStatus::Exited(pid, _)) |
                            Ok(nix::sys::wait::WaitStatus::Signaled(pid, _, _)) => {
                                println!("PID {} exited", pid);
                                if let Some(mut o) = brood.remove(&(pid as u32)) { match o.state {
                                    OffspringState::Infancy => {
                                        if o.attempts + 1 >= cfg.retries {
                                            println!("Ran out of retries...");
                                        } else {
                                            let mut successor = o.respawn(&mut cfg, &mut timer, timer_tx.clone()).unwrap();
                                            successor.attempts = o.attempts + 1;
                                            brood.insert(successor.process.id(), successor);
                                        }
                                    },
                                    OffspringState::Healthy => {
                                        let mut successor = o.respawn(&mut cfg, &mut timer, timer_tx.clone()).unwrap();
                                        successor.replaces = Some(pid as u32);
                                        brood.insert(successor.process.id(), successor);
                                    },
                                    OffspringState::Notified => (),
                                    OffspringState::Dead => {
                                        println!("ERR: double-notified death on {}", pid);
                                    }
                                } };
                            },
                            Ok(nix::sys::wait::WaitStatus::StillAlive) => break,
                            Ok(_) => {
                                println!("Some other thing we don't care about happened: {:?}", res);
                            },
                            Err(nix::Error::Sys(nix::Errno::ECHILD)) => {
                                println!("all children are dead, bailing");
                                run = false;
                                break;
                            },
                            Err(e) => {
                                println!("waitpid err: {}", e);
                                break;
                            },
                        }
                    };
                },
                Signal::HUP => {
                    for (_, o) in brood.iter_mut() {
                        o.signal(sig.unwrap());
                    } },
                Signal::INT | Signal::TERM => {
                    println!("Notifying children...");
                    for (_, o) in brood.iter_mut() {
                        o.terminate(&mut cfg, &mut timer, timer_tx.clone());
                    }
                    run = false;
                },
                _ => ()
            },
        }
        if !run { break; }
    }

    println!("Reaping children... (count={})", brood.len());
    for (_, o) in brood.iter_mut() {
        o.process.wait().ok();
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
    opts.optflag("", "version", "print the version");
    opts.optflag("v", "verbose", "more debugging messages");
    opts.optflag("4", "ipv4-only", "only accept IPv4 connections");
    opts.optflag("6", "ipv6-only", "only accept IPv6 connections");
    opts.optflag("m", "manual", "manual (explicit) acknowledge mode");
    opts.optopt("n", "number", "how many program copies to spawn", "COUNT");
    opts.optmulti("b", "bind", "socket(s) to bind to", "ADDR");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { println!("{}", f.to_string()); print_usage(opts); exit(-1); }
    };          

    if matches.opt_present("help") {
        print_usage(opts);
        return;
    }

    if matches.opt_present("version") {
        println!("einhyrningsins {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if matches.opt_present("4") && matches.opt_present("6") {
        println!("Can't be both IPv4-only and IPv6-only");
        exit(-1);
    }

    //// Parse Configuration
    let (ctrl_req_tx, ctrl_req_rx): (Sender<CtrlRequest>, Receiver<CtrlRequest>) = chan::async();
    let mut cfg = EinConfig{
        count: 1,
        childhood: Duration::seconds(3),
        graceperiod: Duration::seconds(3),
        retries: 3,
        bind_fds: vec![],
        ipv4_only: matches.opt_present("4"),
        ipv6_only: matches.opt_present("6"),
        manual_ack: matches.opt_present("m"),
        cmd: Command::new(""),
        ctrl_req_rx: ctrl_req_rx,
    };

    if let Some(n) = matches.opt_str("number") {
        cfg.count = u64::from_str(&n).expect("number arg should be an integer");
    }

    //// Bind Sockets

    // Control socket first
    let ctrl_path = Path::new("/tmp/einhorn.sock");
    // XXX: handle this more gracefully (per-process)
    if ctrl_path.exists() {
        fs::remove_file(&ctrl_path).unwrap();
    }
    println!("Binding control socket to: {:?}", ctrl_path);
    let ctrl_listener = UnixListener::bind(ctrl_path).unwrap();
    // XXX: set mode/permissions/owner?

    // These will be tuples: (SocketAddr, SO_REUSEADDR, O_NONBLOCK)
    let sock_confs: Vec<(SocketAddr, bool, bool)> = matches.opt_strs("bind").iter().map(|b| {
        let mut r = false;
        let mut n = false;
        let mut addr_chunks = b.split(',');
        let sock_str = addr_chunks.next().unwrap();
        let mut sock_addrs = sock_str.to_socket_addrs().unwrap();
        // ugly
        let sock = if cfg.ipv4_only {
            let mut sock_addrs = sock_addrs.filter(
                |sa| if let SocketAddr::V4(_) = *sa { true } else { false });
            sock_addrs.next().expect("Couldn't bind as IPv4")
        } else if cfg.ipv6_only {
            let mut sock_addrs = sock_addrs.filter(
                |sa| if let SocketAddr::V6(_) = *sa { true } else { false });
            sock_addrs.next().expect("Couldn't bind as IPv6")
        } else {
            sock_addrs.next().expect("Couldn't bind socket")
        };
        for subarg in addr_chunks { match subarg {
            "r" => r = true,
            "n" => n = true,
            "" => (),
            _ => { println!("Unknown socket arg '{}', I only know about 'n' and 'r'. Try --help", subarg);
                   exit(-1); },
        }}
        (sock, r, n)
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

    let binds: Vec<(TcpListener, bool, bool)> = sock_confs.iter().map(|t| {
        let sa = t.0; let r = t.1; let n = t.2; // ugly
        (TcpListener::bind(sa).unwrap(), r, n)
    }).collect();

    let mut cmd = Command::new(&program_and_args[0]);
    cmd.args(&program_and_args[1..]);

    cfg.bind_fds = binds.into_iter().map(|t| {
        let b = t.0; let r = t.1; let n = t.2;  // ugly
        let orig_fd = b.into_raw_fd();
        // Duplicate, which also clears the CLOEXEC flag
        let fd = nix::unistd::dup(orig_fd).unwrap();
        if r {
            nix::sys::socket::setsockopt(fd, nix::sys::socket::sockopt::ReuseAddr, &true).unwrap();
        }
        if n {
            nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::O_NONBLOCK)).unwrap();
        }
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
    // TODO: Should mask others here? START, etc?
    let signal_rx = chan_signal::notify(&[Signal::INT,
                                          Signal::TERM,
                                          Signal::CHLD, // NB: PR has been submitted
                                          Signal::USR2,
                                          Signal::HUP]);

    //// Start Constrol Socket Thread
    thread::spawn(move || ctrl_socket_serve(ctrl_listener, ctrl_req_tx));

    shepard(cfg, signal_rx);
    exit(0);
}

fn ctrl_socket_handle(stream: UnixStream, ctrl_req_tx: Sender<CtrlRequest>) {
    let reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);
    for rawline in reader.lines() {

        let rawline = rawline.unwrap();
        println!("Got line: {}", rawline);
        if rawline.len() == 0 {
            continue;
        }

        // Parse message
        let req_action = if let Ok(msg) = json::parse(&rawline) {
            match msg["command"].as_str() {
                Some("worker:ack") => {
                    CtrlAction::ManualAck(msg["pid"].as_u32().unwrap())
                },
                Some("signal") => {
                    CtrlAction::SigAll(match msg["args"][0].as_str() {
                        Some("SIGHUP") | Some("HUP") => Signal::HUP,
                        Some("SIGINT") | Some("INT") => Signal::INT,
                        Some("SIGTERM") | Some("TERM") => Signal::TERM,
                        Some("SIGKILL") | Some("KILL") => Signal::KILL,
                        Some("SIGUSR1") | Some("USR1") => Signal::KILL,
                        Some("SIGUSR2") | Some("USR2") => Signal::USR2,
                        Some("SIGSTOP") | Some("STOP") => Signal::STOP,
                        Some("SIGCONT") | Some("CONT") => Signal::CONT,
                        Some(_) | None => {
                            writer.write_all("\"Missing or unhandled 'signal'\"\n".as_bytes()).unwrap();
                            writer.flush().unwrap();
                            continue;
                        },
                    })
                },
                Some("inc") => CtrlAction::Increment,
                Some("dec") => CtrlAction::Decrement,
                Some("status") => CtrlAction::Status,
                Some("die") => CtrlAction::ShutdownAll,
                Some("upgrade") => CtrlAction::UpgradeAll,
                Some("ehlo") => {
                    writer.write_all("\"Hi there!\"\n\r".as_bytes()).unwrap();
                    writer.flush().unwrap();
                    continue;
                },
                Some("help") => {
                    writer.write_all("\"Command Listing: <TODO>\"\n".as_bytes()).unwrap(); // TODO
                    writer.flush().unwrap();
                    continue;
                },
                Some("version") => {
                    let ver = format!("\"einhyrningsinsctl {}\"\n", env!("CARGO_PKG_VERSION"));
                    writer.write_all(ver.as_bytes()).unwrap();
                    writer.flush().unwrap();
                    continue;
                },
                Some(_) | None => {
                    writer.write_all("\"Missing or unhandled 'command'\"\n".as_bytes()).unwrap();
                    writer.flush().unwrap();
                    continue;
                },
            }
        } else {
            writer.write_all("\"Expected valid JSON!\"\n".as_bytes()).unwrap();
            writer.flush().unwrap();
            continue;
        };

        // Send request
        let (tx, rx): (Sender<String>, Receiver<String>)  = chan::async();
        let req = CtrlRequest{ action: req_action, tx: tx };
        ctrl_req_tx.send(req);

        // Send reply
        let resp = rx.recv().unwrap();
        writer.write_all("\"".as_bytes()).unwrap();
        writer.write_all(resp.as_bytes()).unwrap();
        writer.write_all("\"\n".as_bytes()).unwrap();
        writer.flush().unwrap();
    }
    stream.shutdown(std::net::Shutdown::Both).unwrap();
}

fn ctrl_socket_serve(listener: UnixListener, ctrl_req_tx: Sender<CtrlRequest>) {
    for conn in listener.incoming() {
        match conn{
            Ok(conn) => {
                let tx = ctrl_req_tx.clone();
                thread::spawn(move || ctrl_socket_handle(conn, tx));
            },
            Err(err) => {
                // TODO
                println!("control socket err: {}", err);
                break;
            },
        }
    }
    drop(listener);
}
