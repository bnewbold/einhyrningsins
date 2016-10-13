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


#[derive(Clone, Debug, PartialEq)]
struct EinConfig {
    program: String,
    program_args: Vec<String>,
    count: u64,
    childhood: Duration,
    graceperiod: Duration,
    retries: u64,
    ipv4_only: bool,
    ipv6_only: bool,
    manual_ack: bool,
    ctrl_path: String,
    bind_slugs: Vec<String>,
    env_drops: Vec<String>,
}

struct EinState {
    cmd: Command,
    ctrl_req_rx: Receiver<CtrlRequest>,
    cfg: EinConfig,
    timer: timer::Timer,
    timer_tx: Sender<TimerAction>,
    timer_rx: Receiver<TimerAction>,
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

    pub fn spawn(state: &mut EinState) -> Result<Offspring, String> {
        let mut o = Offspring {
            state: OffspringState::Infancy,
            process: state.cmd.spawn().expect("error spawning"),
            attempts: 0,
            timer_guard: None,
            replaces: None,
        };
        let pid = o.process.id();
        let t_tx = state.timer_tx.clone();
        o.timer_guard = Some(state.timer.schedule_with_delay(state.cfg.childhood, move || {
            t_tx.send(TimerAction::CheckAlive(pid));
        }));
        Ok(o)
    }

    pub fn respawn(&mut self, state: &mut EinState) -> Result<Offspring, String> {
        let mut successor = try!(Offspring::spawn(state));
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

    pub fn terminate(&mut self, state: &mut EinState) {
        if !self.is_active() { return; }
        self.signal(Signal::TERM);
        self.state = OffspringState::Notified;
        let pid = self.process.id();
        let t_tx = state.timer_tx.clone();
        self.timer_guard = Some(state.timer.schedule_with_delay(state.cfg.graceperiod, move || {
            t_tx.send(TimerAction::CheckTerminated(pid));
        }));
    }

    pub fn shutdown(&mut self, state: &mut EinState) {
        if !self.is_active() { return; }
        self.signal(Signal::USR2);
        self.state = OffspringState::Notified;
        let pid = self.process.id();
        let t_tx = state.timer_tx.clone();
        self.timer_guard = Some(state.timer.schedule_with_delay(state.cfg.graceperiod , move || {
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
            Signal::QUIT  => nix::sys::signal::Signal::SIGQUIT,
            Signal::TERM  => nix::sys::signal::Signal::SIGTERM,
            Signal::KILL  => nix::sys::signal::Signal::SIGKILL,
            Signal::TTIN  => nix::sys::signal::Signal::SIGTTIN,
            Signal::TTOU  => nix::sys::signal::Signal::SIGTTOU,
            Signal::USR1  => nix::sys::signal::Signal::SIGUSR1,
            Signal::USR2  => nix::sys::signal::Signal::SIGUSR2,
            Signal::STOP  => nix::sys::signal::Signal::SIGSTOP,
            Signal::CONT  => nix::sys::signal::Signal::SIGCONT,
            _ => { println!("Unexpected signal: {:?}", sig); return; },
        };
        nix::sys::signal::kill(self.process.id() as i32, nix_sig).unwrap();
    }
}

/* * * * * * * *   Main Event Loop   * * * * * * * */
fn shepard(mut state: EinState, signal_rx: Receiver<Signal>) {

    //// birth the initial set of offspring
    let mut brood: HashMap<u32, Offspring> = HashMap::new();
    for _ in 0..state.cfg.count {
        let o = Offspring::spawn(&mut state).unwrap();
        let pid = o.process.id();
        brood.insert(pid, o);
        println!("Spawned: {}", pid); 
    }

    // Ugh, see: http://burntsushi.net/rustdoc/chan/macro.chan_select.html#failure-modes
    let ctrl_req_rx = state.ctrl_req_rx.clone();
    let timer_rx = state.timer_rx.clone();

    //// infinite select() loop over timers, signals
    let mut run = true;
    loop {
        chan_select! {
            timer_rx.recv() -> action => match action.expect("Error with timer thread") {
                TimerAction::CheckAlive(pid) => {
                    // Need to move 'o' out of HashMap here so we can mutate
                    // the map in other ways
                    if let Some(mut o) = brood.remove(&pid) {
                        if !state.cfg.manual_ack && o.state == OffspringState::Infancy {
                            if o.is_active() {
                                println!("{} found to be alive", pid);
                                o.state = OffspringState::Healthy;
                                if let Some(old_pid) = o.replaces {
                                    if let Some(old) = brood.get_mut(&old_pid) {
                                        old.shutdown(&mut state);
                                    }
                                }
                            }
                        } else if state.cfg.manual_ack && o.state == OffspringState::Infancy {
                            println!("{} didn't check in", pid);
                            if o.attempts + 1 >= state.cfg.retries {
                                println!("Ran out of retries...");
                            } else {
                                let mut successor = o.respawn(&mut state).unwrap();
                                successor.attempts = o.attempts + 1;
                                brood.insert(successor.process.id(), successor);
                            }
                            o.terminate(&mut state);
                        } else {
                            println!("Unexpected CheckAlive state! pid={} state={:?}", o.process.id(), o.state);
                        }
                        brood.insert(pid, o);
                    };
                },
                TimerAction::CheckShutdown(pid) => {
                    if let Some(o) = brood.get_mut(&pid) {
                        if o.is_active() {
                            o.terminate(&mut state);
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
                        let o = Offspring::spawn(&mut state).unwrap();
                        let pid = o.process.id();
                        brood.insert(pid, o);
                        req.tx.send(format!("Spawned! Went from {} to {}", state.cfg.count, state.cfg.count+1));
                        state.cfg.count = state.cfg.count+1;
                    },
                    CtrlAction::Decrement => {
                        if state.cfg.count <= 0 {
                            req.tx.send(format!("Already at count=0, no-op"));
                            continue;
                        }
                        let mut done = false;
                        for (_, o) in brood.iter_mut() {
                            if o.is_active() {
                                o.shutdown(&mut state);
                                req.tx.send(format!("Notified! Went from {} to {}", state.cfg.count, state.cfg.count-1));
                                state.cfg.count = state.cfg.count-1;
                                done = true;
                                break;
                            }
                        }
                        if !done {
                            req.tx.send(format!("No live workers to shutdown! :("));
                        }
                    },
                    CtrlAction::SigAll(sig) => {
                        for (_, o) in brood.iter_mut() {
                            o.signal(sig);
                        }
                        req.tx.send(format!("Signalled all children!"));
                    },
                    CtrlAction::ShutdownAll => {
                        let mut pid_list = vec![];
                        for (pid, o) in brood.iter_mut() {
                            if o.is_active() {
                                o.shutdown(&mut state);
                                pid_list.push(pid);
                            }
                        }
                        req.tx.send(format!("Sent shutdown to all children!"));
                    },
                    CtrlAction::UpgradeAll => {
                        let keys: Vec<u32> = brood.keys().map(|x| *x).collect();
                        for pid in keys {
                            let mut successor = {
                                let o = brood.get_mut(&pid).unwrap();
                                if !o.is_active() {
                                    continue;
                                }
                                o.respawn(&mut state).unwrap()
                            };
                            successor.attempts = 0;
                            brood.insert(successor.process.id(), successor);
                        }
                        req.tx.send(format!("Upgrading all children!"));
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
                                        if o.attempts + 1 >= state.cfg.retries {
                                            println!("Ran out of retries...");
                                        } else {
                                            let mut successor = o.respawn(&mut state).unwrap();
                                            successor.attempts = o.attempts + 1;
                                            brood.insert(successor.process.id(), successor);
                                        }
                                    },
                                    OffspringState::Healthy => {
                                        let mut successor = o.respawn(&mut state).unwrap();
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
                    let keys: Vec<u32> = brood.keys().map(|x| *x).collect();
                    for pid in keys {
                        let mut successor = {
                            let o = brood.get_mut(&pid).unwrap();
                            if !o.is_active() {
                                continue;
                            }
                            o.respawn(&mut state).unwrap()
                        };
                        successor.attempts = 0;
                        brood.insert(successor.process.id(), successor);
                    } },
                Signal::TTIN | Signal::TTOU | Signal::USR1 | Signal::STOP | Signal::CONT => {
                    println!("Passing signal to children: {:?}", sig.unwrap());
                    for (_, o) in brood.iter_mut() {
                        o.signal(sig.unwrap());
                    } },
                Signal::INT | Signal::USR2 => {
                    println!("Exiting! Gracefully shutting down children first, but won't wait.");
                    for (_, o) in brood.iter_mut() {
                        o.shutdown(&mut state);
                    }
                    run = false;
                },
                Signal::TERM | Signal::QUIT => {
                    println!("Exiting! Killing children first, but won't wait.");
                    for (_, o) in brood.iter_mut() {
                        o.terminate(&mut state);
                    }
                    run = false;
                },
                default => {
                    println!("Unexpected signal: {:?} (ignoring)", default);
                },
            },
        }
        if !run { break; }
    }

    println!("Reaping children... (count={})", brood.len());
    for (pid, o) in brood.iter() {
        if o.is_active() {
            nix::sys::wait::waitpid(*pid as i32, Some(nix::sys::wait::WNOHANG)).ok();
        }
    }
    println!("Done.");
}

/* * * * * * * *   Setup and CLI   * * * * * * * */

fn print_usage(opts: Options) {
    let brief = "usage:\teinhyrningsins [options] [--] program [program_args]";
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
    opts.optmulti("b", "bind", "socket(s) to bind to (can be repeated)", "ADDR");
    opts.optmulti("", "drop-env-var", "ENV variables to mask (can be repeated)", "ADDR");
    opts.optopt("d", "socket-path", "where to look for control socket (default: /tmp/einhorn.sock)", "PATH");
    opts.optopt("r", "retries", "how many times to attempt spawning", "RETRIES");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { println!("{}\n", f.to_string()); print_usage(opts); exit(-1); }
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
    let path_str = matches.opt_str("socket-path").unwrap_or("/tmp/einhorn.sock".to_string());

    let count = match matches.opt_str("number") {
        Some(n) => u64::from_str(&n).expect("number arg should be an integer"),
        None => 1,
    };

    let retries = match matches.opt_str("retries") {
        Some(n) => u64::from_str(&n).expect("retries arg should be an integer"),
        None => 1,
    };

    let bind_slugs = matches.opt_strs("bind");
    let env_drops = matches.opt_strs("drop-env-var");
    let ipv4_only = matches.opt_present("4");
    let ipv6_only = matches.opt_present("6");
    let manual_ack =  matches.opt_present("m");

    let program_and_args = if !matches.free.is_empty() {
        matches.free
    } else {
        println!("Missing program to run (try --help)");
        exit(-1);
    };
    let mut program_and_args = program_and_args.into_iter();
    let cfg = EinConfig {
        program: program_and_args.next().unwrap(),
        program_args: program_and_args.collect(),
        count: count,
        childhood: Duration::seconds(3),
        graceperiod: Duration::seconds(3),
        retries: retries,
        ipv4_only: ipv4_only,
        ipv6_only: ipv6_only,
        manual_ack: manual_ack,
        ctrl_path: path_str,
        bind_slugs: bind_slugs,
        env_drops: env_drops,
    };

    // Control socket first; not same scope as other state
    // XXX: handle this more gracefully (per-process)
    let tmp = cfg.ctrl_path.clone();
    let ctrl_path = Path::new(&tmp);
    if ctrl_path.exists() {
        fs::remove_file(&ctrl_path).unwrap();
    }

    println!("Binding control socket to: {:?}", ctrl_path);
    let ctrl_listener = UnixListener::bind(ctrl_path).unwrap();
    // XXX: set mode/permissions/owner?

    let (ctrl_req_tx, ctrl_req_rx): (Sender<CtrlRequest>, Receiver<CtrlRequest>) = chan::async();

    //// Listen for signals (before any fork())
    println!("Registering signal handlers...");
    let signal_rx = chan_signal::notify(&[Signal::HUP,
                                          Signal::INT,
                                          Signal::QUIT,
                                          Signal::TERM,
                                          Signal::PIPE,
                                          Signal::ALRM,
                                          Signal::CHLD, // NB: PR has been submitted
                                          Signal::TTIN,
                                          Signal::TTOU,
                                          Signal::USR1,
                                          Signal::USR2,
                                          Signal::STOP,
                                          Signal::CONT]);

    let state = match init(cfg, ctrl_req_rx) {
        Ok(s) => s,
        Err(e) => { println!("{}", e); exit(-1); },
    };

    //// Start Constrol Socket Thread
    thread::spawn(move || ctrl_socket_serve(ctrl_listener, ctrl_req_tx));

    //// State Event Loop
    shepard(state, signal_rx);
    exit(0);
}

// Initializes config into state
fn init(cfg: EinConfig, ctrl_req_rx: Receiver<CtrlRequest>) -> Result<EinState, String> {

    // These will be tuples: (SocketAddr, SO_REUSEADDR, O_NONBLOCK)
    let sock_confs: Vec<(SocketAddr, bool, bool)> = cfg.bind_slugs.iter().map(|b| {
        let mut r = false;
        let mut n = false;
        let mut addr_chunks = b.split(',');
        let sock_str = addr_chunks.next().unwrap(); // safe
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

    // Configure logging
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

    let mut cmd = Command::new(cfg.program.clone());
    cmd.args(&cfg.program_args);
    for var in &cfg.env_drops {
        cmd.env_remove(var);
    }

    let bind_fds: Vec<RawFd> = binds.into_iter().map(|t| {
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

    cmd.env("EINHORN_FD_COUNT", bind_fds.len().to_string());
    // This iterator destroys the TcpListeners
    for (i, fd) in bind_fds.iter().enumerate() {
        cmd.env(format!("EINHORN_FD_{}", i), fd.to_string());
    }

    // create timer thread
    let timer = timer::Timer::new();
    let (timer_tx, timer_rx): (Sender<TimerAction>, Receiver<TimerAction>) = chan::async();

    Ok(EinState {
        cmd: cmd,
        ctrl_req_rx: ctrl_req_rx,
        cfg: cfg,
        timer: timer,
        timer_tx: timer_tx,
        timer_rx: timer_rx,
    })
}

/* * * * * * * *   Control Socket Server   * * * * * * * */

const CTRL_SHELL_USAGE: &'static str = r#"Command Listing:

    inc             increments number of children
    dec             decrements number of children
    upgrade         replaces all children with new spawns, gracefully
    die             kills all children gracefully, then exits
    shutdown        kills all children gracefully, then exits
    signal SIG      sends signal SIG to all children
    status          shows summary state of children
    help            prints this help message
    version         prints (master) version
"#;

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
                        Some("SIGHUP")  | Some("HUP")  | Some("hup")  => Signal::HUP,
                        Some("SIGINT")  | Some("INT")  | Some("int")  => Signal::INT,
                        Some("SIGTERM") | Some("TERM") | Some("term") => Signal::TERM,
                        Some("SIGTTIN") | Some("TTIN") | Some("ttin") => Signal::TTIN,
                        Some("SIGTTOU") | Some("TTOU") | Some("ttou") => Signal::TTOU,
                        Some("SIGKILL") | Some("KILL") | Some("kill") => Signal::KILL,
                        Some("SIGUSR1") | Some("USR1") | Some("usr1") => Signal::USR1,
                        Some("SIGUSR2") | Some("USR2") | Some("usr2") => Signal::USR2,
                        Some("SIGSTOP") | Some("STOP") | Some("stop") => Signal::STOP,
                        Some("SIGCONT") | Some("CONT") | Some("cont") => Signal::CONT,
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
                                    //let escaped = json::JsonValue::String(CTRL_SHELL_USAGE.to_string());
                                    //writer.write_all(json::stringify(escaped).as_bytes()).unwrap();

                    let escaped = json::stringify(json::JsonValue::from(CTRL_SHELL_USAGE));
                    writer.write_all(escaped.as_bytes()).unwrap();
                    writer.write_all("\n".as_bytes()).unwrap();
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
