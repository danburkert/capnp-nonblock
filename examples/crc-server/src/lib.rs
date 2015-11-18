extern crate capnp;
extern crate capnp_nonblock;
extern crate crc;
#[macro_use]
extern crate log;
extern crate mio;

mod messages_capnp {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/messages_capnp.rs"));
}

use std::fmt;
use std::io::{
    Write,
};
use std::net::{
    SocketAddr,
    TcpStream,
};

use capnp::Result;
use capnp::message::{
    Builder,
    ReaderOptions,
};
use capnp::serialize::{
    read_message,
    write_message,
};
use capnp_nonblock::MessageStream;
use crc::crc32;
use mio::tcp::TcpListener;
use mio::{
    EventLoop,
    EventSet,
    Handler,
    PollOpt,
    Token,
};
use mio::util::Slab;

use messages_capnp::{
    crc_request,
    crc_response,
};

/// Connects to a CRC server and requests a checksum operation.
pub fn checksum(server_addr: &SocketAddr, data: &[u8]) -> Result<u32> {
    let mut request = Builder::new_default();
    {
        let mut builder = request.init_root::<crc_request::Builder>();
        builder.set_data(data);
    }

    let mut stream = try!(TcpStream::connect(server_addr));
    try!(write_message(&mut stream, &request));
    try!(stream.flush());

    let response = try!(read_message(&mut stream, ReaderOptions::new()));
    let reader = try!(response.get_root::<crc_response::Reader>());
    return Ok(reader.get_crc());
}

const LISTENER: Token = Token(10);
const INITIAL: Token = Token(1);

struct Connection {
    stream: MessageStream<mio::tcp::TcpStream>,
    token: Token,
}

impl Connection {

    fn new(tcp_stream: mio::tcp::TcpStream) -> Connection {
        Connection {
            stream: MessageStream::new(tcp_stream, ReaderOptions::new()),
            token: INITIAL,
        }
    }

    fn set_token(&mut self, token: Token) {
        assert_eq!(self.token, INITIAL);
        self.token = token;
    }

    fn readable(&mut self) -> Result<()> {
        while let Some(message) = try!(self.stream.read_message()) {
            let data = try!(try!(message.get_root::<crc_request::Reader>()).get_data());
            let crc = crc32::checksum_castagnoli(data);
            info!("computing checksum of '{:?}' -> 0x{:X}", data, crc);

            let mut response = Builder::new_default();
            {
                let mut builder = response.init_root::<crc_response::Builder>();
                builder.set_crc(crc);
            }

            try!(self.stream.write_message(response));
        }
        Ok(())
    }

    fn writable(&mut self) -> Result<()> {
        trace!("connection writable: {:?}", self);
        try!(self.stream.write());
        Ok(())
    }

    fn register(&mut self, event_loop: &mut EventLoop<CrcServer>) -> Result<()> {
        trace!("registering connection {:?}", self);
        assert!(self.token != INITIAL);
        let event_set = EventSet::all() - EventSet::writable();
        try!(event_loop.register_opt(self.stream.inner(),
                                     self.token,
                                     event_set,
                                     PollOpt::edge() | PollOpt::oneshot()));
        Ok(())
    }

    fn reregister(&mut self, event_loop: &mut EventLoop<CrcServer>) -> Result<()> {
        trace!("reregistering connection {:?}", self);
        assert!(self.token != INITIAL);
        let mut event_set = EventSet::all();
        if !self.stream.has_queued_outbound_messages() {
            event_set = event_set - EventSet::writable()
        };
        try!(event_loop.reregister(self.stream.inner(),
                                   self.token,
                                   event_set,
                                   PollOpt::edge() | PollOpt::oneshot()));
        Ok(())
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.stream.inner().peer_addr() {
            Ok(addr) => write!(f, "Connection({})", addr),
            Err(_) => write!(f, "Connection(<unknown>)"),
        }
    }
}

pub struct CrcServer {
    listener: TcpListener,
    connections: Slab<Connection>,
}

impl CrcServer {

    pub fn run(addr: SocketAddr) -> Result<()> {
        let mut event_loop = try!(EventLoop::<CrcServer>::new());
        let listener = try!(TcpListener::bind(&addr));
        try!(event_loop.register(&listener, LISTENER));

        let mut server = CrcServer {
            listener: listener,
            connections: Slab::new_starting_at(Token(11), 128),
        };

        event_loop.run(&mut server).map_err(From::from)
    }

    fn accept_connection(&mut self, event_loop: &mut EventLoop<CrcServer>) -> Result<()> {
        while let Some(stream) = try!(self.listener.accept()) {
            let token = match self.connections.insert(Connection::new(stream)) {
                Ok(token) => token,
                Err(conn) => {
                    warn!("connection limit reached; dropping connection {:?}", conn);
                    return Ok(());
                }
            };

            self.connections[token].set_token(token);
            match self.connections[token].register(event_loop) {
                Ok(_) => info!("new connection registered: {:?}", self.connections[token]),
                Err(error) => {
                    self.reset_connection(token);
                    return Err(error);
                },
            }
        }
        Ok(())
    }

    fn connection_ready(&mut self,
                        event_loop: &mut EventLoop<CrcServer>,
                        token: Token,
                        events: EventSet)
                        -> Result<()> {
        let connection = &mut self.connections[token];
        trace!("connection ready: {:?}, events: {:?}", connection, events);
        if events.is_readable() {
            try!(connection.readable());
        }

        if events.is_writable() {
            try!(connection.writable());
        }

        connection.reregister(event_loop)
    }

    fn reset_connection(&mut self, token: Token) {
        let connection = self.connections.remove(token).expect("unable to find connection");
        info!("connection reset: {:?}", connection);
    }
}

impl Handler for CrcServer {
    type Timeout = ();
    type Message = ();
    fn ready(&mut self, event_loop: &mut EventLoop<CrcServer>, token: Token, events: EventSet) {

        if events.is_hup() || events.is_error() {
            assert!(token != LISTENER, "unexpected error event on listener socket");
            self.reset_connection(token);
        } else if token == LISTENER {
            self.accept_connection(event_loop)
                .unwrap_or_else(|error| warn!("unable to accept connection: {}", error));
        } else {
            self.connection_ready(event_loop, token, events)
                .unwrap_or_else(|error| {
                    warn!("{:?}: connection error: {}", self.connections[token], error);
                    self.reset_connection(token);
                });
        }
    }
}
