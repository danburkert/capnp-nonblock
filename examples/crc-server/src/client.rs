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

use messages_capnp::{
    crc_request,
    crc_response,
};

