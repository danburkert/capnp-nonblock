#![feature(alloc, heap_api, oom, read_exact)]

extern crate alloc;
extern crate capnp;
#[macro_use]
extern crate nom;

#[cfg(test)]
extern crate quickcheck;
extern crate byteorder;

mod buf;

#[cfg(test)]
mod test_utils;

use std::cmp;
use std::collections::VecDeque;
use std::io;
use std::mem;
use std::result;

use byteorder::{ByteOrder, LittleEndian};
use capnp::Word;
use capnp::message::{Builder, Reader, ReaderOptions, ReaderSegments};
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

pub type OutboundMessage = capnp::message::Builder<capnp::message::HeapAllocator>;

/// A message reader wraps an instance of
/// [`Read`](https://doc.rust-lang.org/stable/std/io/trait.Read.html) and
/// provides an iterator over the messages. `MessageStream` performs it's own
/// internal buffering, so the provided `Read` instance need not be buffered.
///
/// The messages must be in the standard uncompressed Cap'n Proto
/// [stream format](https://capnproto.org/encoding.html#serialization-over-a-stream).
///
/// `MessageStream` attempts to reduce the number of required allocations when
/// reading messages by allocating memory in large chunks, which it loans out to
/// messages via reference counting. The reference counting is not thread safe,
/// so messages read by `MessageStream` may not be sent or shared across thread
/// boundaries.
pub struct MessageStream<S> {
    stream: S,
    options: ReaderOptions,

    /// The current read buffer.
    buf: MutBuf,
    /// The current read offset.
    buf_offset: usize,
    /// The segment sizes of the remaining segments of message currently being
    /// read, in reverse order.
    remaining_segments: Vec<usize>,
    /// The segments of the message currently being read.
    segments: Vec<Buf>,

    /// Queue of outbound messages which have not yet begun being written to the
    /// stream.
    write_queue: VecDeque<OutboundMessage>,

    /// The outbound message currently being written to the stream.
    current_write: Option<OutboundMessage>,

    /// The serialized segment table of the message currently being written to
    /// the stream.
    current_segment_table: Vec<u8>,

    /// The progress of the current write.
    ///
    /// The first corresponds to the segment currently being written, offset by
    /// 1, or 0 if the segment table is being written. The second corresponds to
    /// the offset within the current segment.
    current_segment: (usize, usize),
}

impl <S> MessageStream<S> {
    pub fn new(stream: S, options: ReaderOptions) -> MessageStream<S> {
        MessageStream {
            stream: stream,
            options: options,
            buf: MutBuf::new(),
            buf_offset: 0,
            remaining_segments: Vec::new(),
            segments: Vec::new(),
            write_queue: VecDeque::new(),
            current_segment_table: Vec::new(),
            current_write: None,
            current_segment: (0, 0),
        }
    }
}

impl <S> MessageStream<S> where S: io::Read {

    /// Reads the segment table, populating the `remaining_segments` field of the
    /// reader on success.
    fn read_segment_table(&mut self) -> Result<()> {
        let MessageStream {
            ref mut stream,
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
                        nom::Needed::Unknown => 8,
                        nom::Needed::Size(size) => cmp::max(8, size),
                    };
                    try!(buf.fill_or_replace(stream, buf_offset, amount));
                },
            }
        }

        *buf_offset += (1 + remaining_segments.len() / 2) * 8;

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
        let MessageStream {
            ref mut stream,
            ref mut buf,
            ref mut buf_offset,
            ..
        } = *self;
        try!(buf.fill_or_replace(stream, buf_offset, len));
        let buf = buf.buf(*buf_offset, len);
        *buf_offset += len;
        Ok(buf)
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

/// Serializes the segment table for the provided segments.
fn serialize_segment_table(segment_table: &mut Vec<u8>, segments: &[&[Word]]) {
    segment_table.clear();

    let mut buf: [u8; 4] = [0; 4];

    <LittleEndian as ByteOrder>::write_u32(&mut buf[..], segments.len() as u32 - 1);
    segment_table.extend(&buf);

    for segment in segments {
        <LittleEndian as ByteOrder>::write_u32(&mut buf[..], segment.len() as u32);
        segment_table.extend(&buf);
    }

    if segments.len() % 2 == 0 {
        segment_table.extend(&[0, 0, 0, 0]);
    }
}

/// Like Write::write_all, but increments `offset` after every successful
/// write.
fn write_segment<W>(write: &mut W, mut buf: &[u8], offset: &mut usize) -> io::Result<()>
where W: io::Write {
    while !buf.is_empty() {
        match write.write(buf) {
            Ok(0) => return result::Result::Err(io::Error::new(io::ErrorKind::WriteZero,
                                                                "failed to write whole message")),
            Ok(n) => { *offset += n; buf = &buf[n..] },
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn write_message<W>(write: &mut W,
                    segment_table: &[u8],
                    segments: &[&[Word]],
                    current_segment: &mut (usize, usize))
                    -> io::Result<()>
where W: io::Write {
    let (ref mut segment_index, ref mut segment_offset) = *current_segment;

    if *segment_index == 0 {
        try!(write_segment(write, &segment_table[*segment_offset..], segment_offset));
        *segment_offset = 0;
        *segment_index += 1;
    }

    for segment in &segments[(*segment_index - 1)..] {
        try!(write_segment(write,
                           &Word::words_to_bytes(segment)[*segment_offset..],
                           segment_offset));
        *segment_offset = 0;
        *segment_index += 1;
    }
    Ok(())
}

impl <S> MessageStream<S> where S: io::Write {

    /// Writes queued messages to the stream. This should be called when the
    /// stream is in non-blocking mode and writable.
    pub fn write(&mut self) -> io::Result<()> {

        let MessageStream {
            ref mut stream,
            ref mut write_queue,
            ref mut current_write,
            ref mut current_segment_table,
            ref mut current_segment,
            ..
        } = *self;

        loop {
            // Get the current message, otherwise pop the next message from the
            // queue and serialize a new segment table. If the queue is empty, return.
            let message: &OutboundMessage = if let Some(ref message) = *current_write {
                message
            } else {
                *current_write = write_queue.pop_front();
                match current_write.as_ref() {
                    Some(message) => {
                        serialize_segment_table(current_segment_table,
                                                &*message.get_segments_for_output());
                        *current_segment = (0, 0);
                        message
                    },
                    None => return Ok(()),
                }
            };

            let segments = &*message.get_segments_for_output();

            match write_message(stream, current_segment_table, segments, current_segment) {
                Err(ref error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Ok(_) => continue,
                error => return error,
            }
        }
    }

    /// Queue message for write.
    ///
    /// This method optimistically begins writing to the stream if there is no
    /// message currently being written. This is necessary for the blocking
    /// stream case, and efficient in the non-blocking case as well, since it is
    /// likely that the stream is writable.
    pub fn write_message(&mut self, message: OutboundMessage) -> io::Result<()> {
        self.write_queue.push_back(message);

        if self.current_write.is_none() {
            self.write()
        } else {
            Ok(())
        }
    }
}

impl <S> Iterator for MessageStream<S> where S: io::Read {
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
        MessageStream,
        parse_segment_table,
        serialize_segment_table,
        write_message,
    };

    use test_utils::*;

    use std::io::Cursor;

    use capnp::{Word, message};
    use capnp::message::ReaderSegments;
    use quickcheck::{quickcheck, TestResult};

    #[test]
    fn test_parse_segment_table() {
        fn compare(expected: &[usize], buf: &[u8]) {
            let mut actual = Vec::new();
            assert!(parse_segment_table(buf, &mut actual).is_done());
            assert_eq!(expected, &*actual);
        }

        compare(&[0 * 8],
                &[0,0,0,0,   // 1 segments
                  0,0,0,0]); // 0 words

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

    #[test]
    fn check_read_segments() {
        fn read_segments(segments: Vec<Vec<Word>>) -> TestResult {
            if segments.len() == 0 { return TestResult::discard(); }
            let mut cursor = Cursor::new(Vec::new());

            write_message_segments(&mut cursor, &segments);
            cursor.set_position(0);

            let mut message_reader = MessageStream::new(&mut cursor, message::ReaderOptions::new());
            let message = message_reader.next().unwrap().unwrap();
            let result_segments = message.into_segments();

            TestResult::from_bool(segments.iter().enumerate().all(|(i, segment)| {
                &segment[..] == result_segments.get_segment(i as u32).unwrap()
            }))
        }

        quickcheck(read_segments as fn(Vec<Vec<Word>>) -> TestResult);
    }

    #[test]
    fn check_write_segments() {
        fn write_segments(segments: Vec<Vec<Word>>) -> TestResult {
            if segments.len() == 0 { return TestResult::discard(); }
            let mut cursor = Cursor::new(Vec::new());
            let mut expected_cursor = Cursor::new(Vec::new());

            write_message_segments(&mut expected_cursor, &segments);
            expected_cursor.set_position(0);

            {
                let borrowed_segments: &[&[Word]] = &segments.iter()
                                                            .map(|segment| &segment[..])
                                                            .collect::<Vec<_>>()[..];
                let mut segment_table = Vec::new();
                serialize_segment_table(&mut segment_table, borrowed_segments);
                let mut current_segment = (0, 0);

                write_message(&mut cursor,
                              &segment_table[..],
                              borrowed_segments,
                              &mut current_segment).unwrap();
            }

            TestResult::from_bool(expected_cursor.into_inner() == cursor.into_inner())
        }

        quickcheck(write_segments as fn(Vec<Vec<Word>>) -> TestResult);
    }
}
