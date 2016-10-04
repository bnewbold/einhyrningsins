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

extern crate getopts;
extern crate log;
extern crate env_logger;
extern crate nix;

use std::env;
use std::u64;
use std::str::FromStr;
use std::process::exit;
use std::process::Command;
use std::process::Child;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::ToSocketAddrs;
use getopts::Options;

use std::os::unix::io::IntoRawFd;

fn run(binds: Vec<TcpListener>, mut prog: Command, number: u64) {

    prog.env("EINHORN_FD_COUNT", binds.len().to_string());
    // This iterator destroys the TcpListeners
    for (i, b) in binds.into_iter().enumerate() {
        let orig_fd = b.into_raw_fd();
        // Duplicate, which also clears the CLOEXEC flag
        //let fd = nix::fcntl::fcntl(nix::fcntl::FcntlArg::F_DUPFD(orig_fd)).unwrap();
        let fd = nix::unistd::dup(orig_fd).unwrap();
        println!("fd={} FD_CLOEXEC={}", fd, nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).unwrap());
        prog.env(format!("EINHORN_FD_{}", i), fd.to_string());
        // NB: is fd getting destroyed here?
    }

    let mut children: Vec<Child> = vec![];
    for _ in 0..number {
        println!("Running!");
        children.push(prog.spawn().expect("error spawning"));
    }

    println!("Waiting for all children to die");
    for mut c in children {
        c.wait().unwrap();
    }
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

    let number: u64 = match matches.opt_str("number") {
        Some(n) => u64::from_str(&n).expect("number arg should be an integer"),
        None => 1
    };

    let sock_addrs: Vec<SocketAddr> = matches.opt_strs("bind").iter().map(|b| {
        //let sa: SocketAddr = b.to_socket_addrs().unwrap().next().unwrap();
        //sa
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

    run(binds, prog, number);
    exit(0);
}
