# capnp-nonblock

A Rust [Cap'n Proto](https://capnproto.org/) message serializer and deserializer
that works with non-blocking streams.

[Documentation](https://danburkert.github.io/capnp-nonblock/capnp_nonblock/index.html)

[![Status](https://travis-ci.org/danburkert/capnp-nonblock.svg?branch=master)](https://travis-ci.org/danburkert/capnp-nonblock)

## Example

An example of using Cap'n Proto messages with a simple
[MIO](https://github.com/carllerche/mio) server is provided. The server receives
messages containing data from clients, computes a checksum of the data, and
returns the checksum to the client.

```bash

# Start the example server
$ cargo run --example crc -- server

# In a different terminal, send data to the server to be checksummed
$ printf "foo bar baz" | cargo run --example crc — checksum
> 0x5F5DCE54
```

## License

`capnp-nonblock` is primarily distributed under the terms of both the MIT
license and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2015 Dan Burkert.
