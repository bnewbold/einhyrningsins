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
extern crate nix;
extern crate log;
extern crate env_logger;

use std::env;
use std::process::exit;
use getopts::Options;

fn print_usage(opts: Options) {
    let brief = "usage:\teinhyrningsins [options] program"; // XXX:
    println!("");
    print!("{}", opts.usage(&brief));
}

fn main() {
    println!("Hello, world!");

    let args: Vec<String> = env::args().collect();

    let mut opts = Options::new();
    opts.optflag("h", "help", "print this help menu");
    opts.optflag("v", "verbose", "more debugging messages");
    opts.optflag("n", "number", "how many program copies to spawn");
    opts.optflag("b", "bind", "how many program copies to spawn");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { println!("{}", f.to_string()); print_usage(opts); exit(-1); }
    };          

    if matches.opt_present("h") {
        print_usage(opts);
        return;
    }

    let mut builder = env_logger::LogBuilder::new();
    builder.parse("INFO");
    if env::var("RUST_LOG").is_ok() {
        builder.parse(&env::var("RUST_LOG").unwrap());
    }
    builder.init().unwrap();
}
