//  einhyrningsinsctl: controller/shell for einhyrningsins
//  Copyright (C) 2016  Bryan Newbold <bnewbold@robocracy.org>
//
//  This program is free software: you can redistribute it and/or modify
//  it under the terms of the GNU General Public License as published by
//  the Free Software Foundation, either version 3 of the License, or
//  (at your option) any later version.
//
//  This program is distributed in the hope that it will be useful,
//  but WITHOUT ANY WARRANTY; without even the implied warranty of
//  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
//  GNU General Public License for more details.
//
//  You should have received a copy of the GNU General Public License
//  along with this program.  If not, see <http://www.gnu.org/licenses/>.
//

#[macro_use]
extern crate json;

extern crate getopts;
extern crate nix;
extern crate timer;
extern crate time;
extern crate chan_signal;
extern crate rustyline;

use std::io;
use std::io::prelude::*;
use std::io::{BufReader, BufWriter};
use std::env;
use std::path::Path;
use std::process::exit;
use std::os::unix::net::UnixStream;
use getopts::Options;

use rustyline::error::ReadlineError;
use rustyline::Editor;


// This is the main event loop
fn shell(ctrl_stream: UnixStream) {

    let mut reader = BufReader::new(&ctrl_stream);
    let mut writer = BufWriter::new(&ctrl_stream);

    // `()` can be used when no completer is required
    let mut rl = Editor::<()>::new();

    println!("");
    println!("Welcome to the einhyrningsins shell!");
    println!("Try 'help' if you need it");

    loop {
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(&line);
                if line.is_empty() {
                    continue;
                };
                let mut chunks = line.split(' ');
                let cmd = chunks.nth(0).unwrap();
                let args = chunks.collect();
                match send_msg(&mut reader, &mut writer, cmd, args) {
                    Ok(s) => {
                        println!("{}", s);
                    }
                    Err(e) => {
                        println!("Error sending control message: {}", e);
                        exit(-1);
                    }
                }
            }
            Err(ReadlineError::Interrupted) |
            Err(ReadlineError::Eof) => {
                println!("Caught kill signal (shutting down)");
                break;
            }
            Err(err) => {
                println!("Shell Error: {:?} (shutting down)", err);
                break;
            }
        }
    }
    // drop(ctrl_stream);
}

// This function sends a single request message down the writer, then waits for a reply on the
// reader and prints the result.
fn send_msg(reader: &mut BufRead,
            writer: &mut Write,
            cmd: &str,
            args: Vec<&str>)
            -> io::Result<String> {

    let mut buffer = String::new();
    let mut arg_list = json::JsonValue::new_array();

    for a in args {
        arg_list.push(a).expect("function args");
    }
    let req = object!{
        "command" => cmd,
        "args" => arg_list
    };
    // println!("Sending: {}", req.dump());
    try!(writer.write_all(format!("{}\n", req.dump()).as_bytes()));
    try!(writer.flush());

    try!(reader.read_line(&mut buffer));
    // println!("Got: {}", buffer);
    let reply = match json::parse(&buffer) {
        Ok(obj) => obj,
        Err(_) => return Ok(buffer),
    };
    Ok(match reply.as_str() {
        Some(s) => s.to_string(),
        None => buffer,
    })
}

fn print_usage(opts: Options) {
    let brief = "usage:\teinhyrningsinsctl [options] program";
    println!("");
    print!("{}", opts.usage(&brief));
}

fn main() {

    let args: Vec<String> = env::args().collect();

    let mut opts = Options::new();
    opts.optflag("h", "help", "print this help menu");
    opts.optflag("", "version", "print the version");
    opts.optopt("e",
                "execute",
                "submit this command instead (no shell)",
                "CMD");
    opts.optopt("d",
                "socket-path",
                "where to look for control socket (default: /tmp/einhorn.sock)",
                "PATH");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            println!("{}", f.to_string());
            print_usage(opts);
            exit(-1);
        }
    };

    if matches.opt_present("help") {
        print_usage(opts);
        return;
    }

    if matches.opt_present("version") {
        println!("einhyrningsinsctl {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Bind to Control Socket
    let path_str = matches.opt_str("socket-path").unwrap_or("/tmp/einhorn.sock".to_string());
    let ctrl_path = Path::new(&path_str);
    if !ctrl_path.exists() {
        println!("Couldn't find control socket ({:?})", ctrl_path);
        println!("Is the master process running? Do you need to tell me the correct socket path?");
        exit(-1);
    }
    // println!("Connecting to control socket: {:?}", ctrl_path);
    let ctrl_stream = match UnixStream::connect(ctrl_path) {
        Ok(s) => s,
        Err(e) => {
            println!("Couldn't open socket [{}]: {}", path_str, e);
            exit(-1);
        }
    };

    // Send a test message before continuing
    send_msg(&mut BufReader::new(&ctrl_stream),
             &mut BufWriter::new(&ctrl_stream),
             "ehlo",
             vec![])
        .unwrap();

    match matches.opt_str("execute") {
        Some(cmd) => {
            match send_msg(&mut BufReader::new(&ctrl_stream),
                           &mut BufWriter::new(&ctrl_stream),
                           &cmd,
                           vec![]) {
                Ok(reply) => println!("{}", reply),
                Err(e) => println!("Communications error: {}", e),
            }
        } 
        None => shell(ctrl_stream),
    }
    exit(0);
}
