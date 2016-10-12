/*
 *  einhyrningsinsctl: controller/shell for einhyrningsins
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
extern crate json;

extern crate getopts;
extern crate log;
extern crate env_logger;
extern crate nix;
extern crate timer;
extern crate time;
extern crate chan_signal;
extern crate url;
extern crate rustyline;


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

    loop {
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(&line);
                if line.len() == 0 { continue; };
                let mut chunks = line.split(' ');
                let cmd = chunks.nth(0).unwrap();
                let args = chunks.collect();
                send_msg(&mut reader, &mut writer, cmd, args).unwrap();
            },
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                println!("Quitting...");
                break
            },
            Err(err) => {
                println!("Shell Error: {:?}", err);
                break
            }
        }
    }
}

fn send_msg(reader: &mut BufRead, writer: &mut Write, cmd: &str, args: Vec<&str>) -> Result<String, String> {
    let mut buffer = String::new();
    let mut arg_list = json::JsonValue::new_array();
    for a in args {
        arg_list.push(a).unwrap();
    }
    let req = object!{
        "command" => cmd,
        "args" => arg_list
    };
    //println!("Sending: {}", req.dump());
    writer.write_all(req.dump().as_bytes()).unwrap();
    writer.write_all("\n".as_bytes()).unwrap();
    writer.flush().unwrap();

    reader.read_line(&mut buffer).unwrap();
    //println!("Got: {}", buffer);
    let reply = json::parse(&buffer).unwrap();
    println!("{}", reply.as_str().unwrap());
    Ok(reply.as_str().unwrap().to_string())
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

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { println!("{}", f.to_string()); print_usage(opts); exit(-1); }
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
    let ctrl_path = Path::new("/tmp/einhorn.sock");
    // XXX: handle this more gracefully (per-process)
    if !ctrl_path.exists() {
        println!("Couldn't find control socket: {:?}", ctrl_path);
        exit(-1);
    }
    println!("Connecting to control socket: {:?}", ctrl_path);
    let ctrl_stream = UnixStream::connect(ctrl_path).unwrap();

    send_msg(&mut BufReader::new(&ctrl_stream), &mut BufWriter::new(&ctrl_stream), "ehlo", vec![]).unwrap();

    shell(ctrl_stream);
    exit(0);
}
