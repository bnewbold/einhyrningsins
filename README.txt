
        _       _                      _                 _           
    ___(_)_ __ | |__  _   _ _ __ _ __ (_)_ __   __ _ ___(_)_ __  ___ 
   / _ \ | '_ \| '_ \| | | | '__| '_ \| | '_ \ / _` / __| | '_ \/ __|
  |  __/ | | | | | | | |_| | |  | | | | | | | | (_| \__ \ | | | \__ \
   \___|_|_| |_|_| |_|\__, |_|  |_| |_|_|_| |_|\__, |___/_|_| |_|___/
                      |___/                    |___/                 

                                                ... is einhorn in Rust!

`einhyrningsins` is a socket multiplexer featuring graceful restarts. It runs
multiple copies of a child program, each of which are passed a shared socket
(or multiple shared sockets) to bind(2) to and accept(2) connections from.
Graceful, rolling restarts enable updates of the child program with zero
downtime and no dropped connections.

`einhyrningsins` is a partially-comparible re-implementation of Einhorn (a Ruby
program) in Rust. Einhorn itself derived from Unicorn.  The word
"einhyrningsins" is Icelandic for unicorn.

Read the manual page at:
https://bnewbold.github.io/einhyrningsins/einhyrningsins.1.html

NOTE: `einhyrningsins` is a for-fun hobby project. It is not feature complete,
fully documented, or tested. See also ./TODO and unstructured notes in ./doc/.

Building and Installation
---------------------------

For now both building and installation are done with rust's cargo tool, usually
bundled with the toolchain. If you haven't used rust before, "rustup" is highly
recommended. einhyrningsins builds with the 'stable' compiler, and was
developed against version 1.12 of the toolchain (September 2016). To build and
install:

    cargo build --release
    cargo install

Manpages (in both roff and HTML format) are built using the `ronn` tool, which
is available in many package managers. To build those pages, run:

    make docs

There (currently) isn't an automated way to install the manpages.

Differences from Einhorn
--------------------------

 * Ruby pre-loading is not possible
 * einhyrningsins does not reload *itself* on upgrades (aka restarts)
 * control socket message line format is JSON, not YAML-in-URL-encoding
 * passing control socket file descriptor is unimplemented
 * children start all-at-once, not with a delay between spawns

License
---------
einhyrningsins is Free Software, released under the GPLv3 license. See LICENSE
for full text.
