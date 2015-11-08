#![feature(alloc, heap_api, oom, read_exact)]

extern crate alloc;
extern crate capnp;
#[macro_use]
extern crate nom;

mod buf;

use std::cmp;
use std::io;
use std::result;
use std::mem;

use capnp::Word;
use capnp::message::{Reader, ReaderOptions, ReaderSegments};
use capnp::{Error, Result};
use nom::le_u32;

use buf::{MutBuf, Buf};

pub struct Segments {
    segments: Vec<Buf>,
}

impl ReaderSegments for Segments {
    fn get_segment(&self, id: u32) -> Option<&[Word]> {
        self.segments.get(id as usize).map(|buf| Word::bytes_to_words(&*buf))
    }
}

/// A message reader wraps an instance of
/// [`Read`](https://doc.rust-lang.org/stable/std/io/trait.Read.html) and
/// provides an iterator over the messages. `MessageReader` performs it's own
/// internal buffering, so the provided `Read` instance need not be buffered.
///
/// The messages must be in the standard uncompressed Cap'n Proto
/// [stream format](https://capnproto.org/encoding.html#serialization-over-a-stream).
///
/// `MessageReader` attempts to reduce the number of required allocations by
/// allocating memory in large chunks, which it loans out to messages via
/// reference counting. The reference counting is not thread safe, so messages
/// read by `MessageReader` may not be sent or shared across thread boundaries.
pub struct MessageReader<R> {
    read: R,
    options: ReaderOptions,
    buf: MutBuf,
    buf_offset: usize,
    /// Holds the segment sizes of the remaining segments in the message in
    /// reverse order.
    remaining_segments: Vec<usize>,
    segments: Vec<Buf>,
}

impl <R> MessageReader<R> where R: io::Read {

    pub fn new(read: R, options: ReaderOptions) -> MessageReader<R> {
        MessageReader {
            read: read,
            options: options,
            buf: MutBuf::new(),
            buf_offset: 0,
            remaining_segments: Vec::new(),
            segments: Vec::new(),
        }
    }

    /// Reads the segment table, populating the `remaining_segments` field of the
    /// reader on success.
    fn read_segment_table(&mut self) -> Result<()> {
        let MessageReader {
            ref mut read,
            ref options,
            ref mut buf,
            ref mut buf_offset,
            ref mut remaining_segments,
            ..
        } = *self;

        loop {
            assert!(remaining_segments.is_empty());

            match parse_segment_table(&buf[*buf_offset..], remaining_segments) {
                nom::IResult::Done(..) => break,
                nom::IResult::Error(nom::Err::Code(nom::ErrorKind::Custom(0))) => {
                    return result::Result::Err(Error::new_decode_error("0 segments in message", None));
                },
                nom::IResult::Error(nom::Err::Code(nom::ErrorKind::Custom(segment_count))) => {
                    return result::Result::Err(Error::new_decode_error("too many segments in message",
                                                                       Some(format!("{}", segment_count))));
                }
                nom::IResult::Error(..) => unreachable!(),
                nom::IResult::Incomplete(needed) => {
                    let amount = match needed {
                        nom::Needed::Unknown => 64,
                        nom::Needed::Size(size) => cmp::max(64, size),
                    };
                    try!(buf.fill_or_replace(read, buf_offset, amount));
                },
            }
        }

        let total_len = remaining_segments.iter()
                                          .fold(Some(0u64), |acc, &len| {
                                              acc.and_then(|n| n.checked_add(len as u64))
                                          });
        match total_len {
            Some(len) if len <= options.traversal_limit_in_words * 8 => (),
            other => return result::Result::Err(Error::new_decode_error(
                    "message is too large", Some(format!("{:?}", other.map(|n| n / 8))))),
        }

        remaining_segments.reverse();
        Ok(())
    }

    fn read_segment(&mut self, len: usize) -> Result<Buf> {
        let MessageReader {
            ref mut read,
            ref mut buf,
            ref mut buf_offset,
            ref mut remaining_segments,
            ..
        } = *self;
        try!(buf.fill_or_replace(read, buf_offset, remaining_segments[0]));
        Ok(buf.buf(*buf_offset, len))
    }

    fn read_message(&mut self) -> Result<Reader<Segments>> {
        if self.remaining_segments.is_empty() {
            try!(self.read_segment_table());
        }

        while let Some(&segment_len) = self.remaining_segments.last() {
            let segment = try!(self.read_segment(segment_len));
            self.segments.push(segment);
            // Only pop the segment length once we know there hasn't been an error.
            self.remaining_segments.pop();
        }


        Ok(Reader::new(Segments { segments: mem::replace(&mut self.segments, Vec::new()) },
                       self.options.clone()))
    }
}

impl <R> Iterator for MessageReader<R> where R: io::Read {
    type Item = Result<Reader<Segments>>;

    fn next(&mut self) -> Option<Result<Reader<Segments>>> {
        match self.read_message() {
            Err(Error::Io(ref error)) if error.kind() == io::ErrorKind::WouldBlock => None,
            a => Some(a),
        }
    }
}

/// Parses a segment table into a sequence of segment lengths, and adds the
/// lengths to the provided `Vec`.
///
/// Fails if the number of segments in the table is invalid, returning the
/// number of segments as the error code.
fn parse_segment_table<'a>(input: &'a [u8], lengths: &mut Vec<usize>) -> nom::IResult<&'a [u8], ()> {
    let (mut i, segment_count) = try_parse!(input, le_u32);
    let segment_count = segment_count.wrapping_add(1);
    if segment_count >= 512 || segment_count == 0 {
        return nom::IResult::Error(nom::Err::Code(nom::ErrorKind::Custom(segment_count)));
    }

    for _ in 0..segment_count {
        let (i_prime, segment_len) = try_parse!(i, le_u32);
        // The Cap'n Proto header is in units of 8-byte words; we want bytes.
        lengths.push(segment_len as usize * 8);
        i = i_prime;
    }

    if segment_count % 2 == 0 {
        if i.len() < 4 {
            nom::IResult::Incomplete(nom::Needed::Size(4))
        } else {
            nom::IResult::Done(&i[4..], ())
        }
    } else {
        nom::IResult::Done(i, ())
    }
}

#[cfg(test)]
pub mod test {

    use super::{
        parse_segment_table,
    };

    #[test]
    fn test_parse_segment_table() {
        fn compare(expected: &[usize], buf: &[u8]) {
            let mut actual = Vec::new();
            assert!(parse_segment_table(buf, &mut actual).is_done());
            assert_eq!(expected, &*actual);
        }

        compare(&[0 * 8],
                &[0,0,0,0,   // 1 segments
                  0,0,0,0]); // 0 length

        compare(&[1 * 8],
                &[0,0,0,0,   // 1 segments
                  1,0,0,0]); // 1 word

        compare(&[1 * 8, 1 * 8],
                &[1,0,0,0,   // 2 segments
                  1,0,0,0,   // 1 word
                  1,0,0,0,   // 1 word
                  0,0,0,0]); // padding

        compare(&[1 * 8, 1 * 8, 256 * 8],
                &[2,0,0,0,   // 3 segments
                  1,0,0,0,   // 1 word
                  1,0,0,0,   // 1 word
                  0,1,0,0]); // 256 length

        compare(&[77 * 8, 23 * 8, 1 * 8, 99 * 8],
                &[3,0,0,0,    // 4 segments
                  77,0,0,0,   // 77 word
                  23,0,0,0,   // 23 words
                  1,0,0,0,    // 1 word
                  99,0,0,0,   // 99 words
                  0,0,0,0]);  // padding
    }

    #[test]
    fn test_parse_invalid_segment_table() {
        let mut v = Vec::new();
        assert!(parse_segment_table(&[255,1,0,0], &mut v).is_err());
        assert!(parse_segment_table(&[0,0,0,0], &mut v).is_incomplete());
        assert!(parse_segment_table(&[0,0,0,0, 0,0,0], &mut v).is_incomplete());
        assert!(parse_segment_table(&[1,0,0,0, 0,0,0,0, 0,0,0], &mut v).is_incomplete());
        assert!(parse_segment_table(&[255,255,255,255], &mut v).is_err());
    }
}
