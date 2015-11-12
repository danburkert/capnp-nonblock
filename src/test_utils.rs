//! Test utilities.

use std::io::{self, Read, Write};
use std::cmp;

use capnp::Word;

use byteorder::{ByteOrder, LittleEndian};

/// Writes segments as if they were a Capnproto message.
///
/// This is copied from capnproto-rust, and exists that our read/write format
/// does not differ from the 'canonical' capnp-rust.
pub fn write_message_segments<W>(write: &mut W, segments: &Vec<Vec<Word>>) where W: Write {
    /// Writes a segment table to `write`.
    ///
    /// `segments` must contain at least one segment.
    fn write_segment_table<W>(write: &mut W, segments: &[&[Word]]) -> ::std::io::Result<()>
    where W: Write {
        let mut buf: [u8; 8] = [0; 8];
        let segment_count = segments.len();

        // write the first Word, which contains segment_count and the 1st segment length
        <LittleEndian as ByteOrder>::write_u32(&mut buf[0..4], segment_count as u32 - 1);
        <LittleEndian as ByteOrder>::write_u32(&mut buf[4..8], segments[0].len() as u32);
        try!(write.write_all(&buf));

        if segment_count > 1 {
            for i in 1..((segment_count + 1) / 2) {
                // write two segment lengths at a time starting with the second
                // segment through the final full Word
                <LittleEndian as ByteOrder>::write_u32(&mut buf[0..4],
                                                       segments[i * 2 - 1].len() as u32);
                <LittleEndian as ByteOrder>::write_u32(&mut buf[4..8],
                                                       segments[i * 2].len() as u32);
                try!(write.write_all(&buf));
            }

            if segment_count % 2 == 0 {
                // write the final Word containing the last segment length and padding
                <LittleEndian as ByteOrder>::write_u32(&mut buf[0..4],
                                                       segments[segment_count - 1].len() as u32);
                try!((&mut buf[4..8]).write_all(&[0, 0, 0, 0]));
                try!(write.write_all(&buf));
            }
        }
        Ok(())
    }

    /// Writes segments to `write`.
    fn write_segments<W>(write: &mut W, segments: &[&[Word]]) -> ::std::io::Result<()>
    where W: Write {
        for segment in segments {
            try!(write.write_all(Word::words_to_bytes(segment)));
        }
        Ok(())
    }

    let borrowed_segments: &[&[Word]] = &segments.iter()
                                                 .map(|segment| &segment[..])
                                                 .collect::<Vec<_>>()[..];
    write_segment_table(write, borrowed_segments).unwrap();
    write_segments(write, borrowed_segments).unwrap();
}

/// Wraps a `Read` instance and introduces blocking.
pub struct BlockingRead<R> where R: Read {
    /// The wrapped reader
    read: R,

    /// Number of bytes to read before blocking
    frequency: usize,

    /// Number of bytes read since last blocking
    idx: usize,
}

impl <R> BlockingRead<R> where R: Read {
    pub fn new(read: R, frequency: usize) -> BlockingRead<R> {
        BlockingRead { read: read, frequency: frequency, idx: 0 }
    }
}

impl <R> Read for BlockingRead<R> where R: Read {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.idx == 0 {
            self.idx = self.frequency;
            Err(io::Error::new(io::ErrorKind::WouldBlock, "BlockingRead"))
        } else {
            let len = cmp::min(self.idx, buf.len());
            let bytes_read = try!(self.read.read(&mut buf[..len]));
            self.idx -= bytes_read;
            Ok(bytes_read)
        }
    }
}
