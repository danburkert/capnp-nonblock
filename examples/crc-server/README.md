# crc-server

An example application using `capnp-nonblock` and `mio`. Consists of a server
which acceps Cap'n Proto messages containing data, and returning the CRC-32
checksum of the data.

```bash

# Start the example server
$ cargo run server

# In a different terminal, send data to the server to be checksummed
$ printf "foo bar baz" | cargo run --example crc — checksum
> 0x5F5DCE54
```

## License

`crc-server` is distributed under the Creative Commons Zero license, as well as
the licenses of `capnp-nonblock` (MIT and Apache). The intent of licensing under
CC0 is to waive the requirement of attribution when copying the example.

See [LICENSE-CC0](LICENSE-CC0) for details.
