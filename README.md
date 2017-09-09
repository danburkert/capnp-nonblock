# capnp-nonblock

A Rust [Cap'n Proto](https://capnproto.org/) message serializer and deserializer
that works with non-blocking streams.

[Documentation](https://danburkert.github.io/capnp-nonblock/capnp_nonblock/index.html)

[![Status](https://travis-ci.org/danburkert/capnp-nonblock.svg?branch=master)](https://travis-ci.org/danburkert/capnp-nonblock)

## Deprecated

This crate is effectively retired; it hails from a time before
[futures](https://crates.io/crates/futures), [Tokio](https://tokio.rs/) and
[capnp-futures](https://crates.io/crates/capnp-futures). Existing projects
utilizing this crate are encouraged to move to `capnp-futures`. Bug-fix pull
requests are accepted, but no new features will be released.

## Example

An [example](examples/crc-server) of using Cap'n Proto messages with a simple
[MIO](https://github.com/carllerche/mio) server is provided.

## License

`capnp-nonblock` is primarily distributed under the terms of both the MIT
license and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2015 Dan Burkert.
