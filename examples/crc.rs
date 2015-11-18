//! An example of using `capnp-nonblock` with MIO.
//!
//! This example consists of a service that computes a checksum on arbitrary
//! data. Messages are in a simple Cap'n Proto format. The client sends a
//! message with the data to checksum to the server, and the server returns the
//! checksum.
//!
//! The server is single-threaded, using a MIO event loop with non-blocking
//! sockets. The client uses standard blocking sockets.

extern crate crc_server;
extern crate docopt;
extern crate env_logger;
extern crate rustc_serialize;

use std::fmt::Display;
use std::io::{self, Read};
use std::net::{
    SocketAddr,
    ToSocketAddrs,
};
use std::ops::Deref;

use docopt::Docopt;

const USAGE: &'static str = "
A checksum server and client.

The server is a long-running processes that accepts data from clients and
returns a checksum value.

Commands:

  server    starts a checksum server.

  checksum  reads data from stdin and sends it to a checksum server for
            processing. Prints the checksum result.

Usage:
  crc server [--address=<addr>]
  crc checksum [--address=<addr>]

Options:
  --address=<addr>  The server address [default: 127.0.0.1:8989].
  -h --help         Show a help message.
";

#[derive(Debug, RustcDecodable)]
struct Args {
    cmd_server: bool,
    cmd_checksum: bool,

    flag_address: String,
}

fn main() {
    let _ = env_logger::init();
    let args: Args = Docopt::new(USAGE)
                            .and_then(|d| d.decode())
                            .unwrap_or_else(|e| e.exit());

    let address = parse_addr(args.flag_address);

    if args.cmd_server {
        server(address);
    } else if args.cmd_checksum {
        checksum(address);
    }
}

/// Parses a socket address from a string, or panics with an error message.
fn parse_addr<S>(addr: S) -> SocketAddr where S: Deref<Target=str> + Display {
    addr.to_socket_addrs()
        .ok()
        .expect(&format!("unable to parse socket address: {}", addr))
        .next()
        .unwrap()
}

fn server(addr: SocketAddr) {
    crc_server::CrcServer::run(addr).unwrap();
}

fn checksum(addr: SocketAddr) {
    let mut data = Vec::new();
    io::stdin().read_to_end(&mut data).unwrap();
    let crc = crc_server::checksum(&addr, &data[..]).unwrap();
    println!("0x{:X}", crc);
}
