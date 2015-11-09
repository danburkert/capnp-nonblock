use alloc::{self, heap};
use std::{cmp, io, mem, ops, ptr, slice};

use std::io::Write;

/// Default buffer size. Perhaps this should be tunable.
const BUF_SIZE: usize = 4096;

/// A reference counted slab allocator.
///
/// `MutBuf` keeps an internal byte buffer to which it allows bytes to be
/// written. The buffer is fixed size, and append only. The bytes may be shared
/// as owned `Buf` instances.
///
/// The reference counting mechanism of `MutBuf` is not threadsafe, so instances
/// may not be shared or sent across thread boundaries.
pub struct MutBuf {
    raw: RawBuf,
    offset: usize,
}

impl io::Write for MutBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        unsafe {
            let count = cmp::min(buf.len(), self.raw.len() - self.offset);
            ptr::copy_nonoverlapping(buf.as_ptr(),
                                     self.raw.buf().offset(self.offset as isize),
                                     count);
            self.offset += count;
            Ok(count)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MutBuf {

    pub fn new() -> MutBuf {
        MutBuf::with_capacity(BUF_SIZE)
    }

    pub fn with_capacity(cap: usize) -> MutBuf {
        MutBuf {
            raw: RawBuf::new(cap),
            offset: 0,
        }
    }

    pub fn buf(&self, offset: usize, len: usize) -> Buf {
        unsafe {
            assert!(offset + len <= self.offset);
            Buf {
                raw: self.raw.clone(),
                ptr: self.raw.buf().offset(offset as isize),
                len: len,
            }
        }
    }

    /// Attempts to fill the buffer with at least `amount` bytes from `read`.
    /// The remaining capacity of the buffer must exceed `amount`.
    fn fill<R>(&mut self, read: &mut R, amount: usize) -> io::Result<()> where R: io::Read {
        unsafe {
            let remaining_capacity = self.raw.len() - self.offset;
            assert!(remaining_capacity >= amount);
            let mut buf = slice::from_raw_parts_mut(self.raw.buf().offset(self.offset as isize),
                                                    remaining_capacity);
            let target_offset = self.offset + amount;
            while self.offset < target_offset {
                match try!(read.read(&mut buf)) {
                    0 => return Result::Err(io::Error::new(io::ErrorKind::UnexpectedEOF,
                                                           "failed to fill whole buffer")),
                    n => {
                        self.offset += n;
                        let mut tmp = buf;
                        buf = &mut tmp[n..];
                    },
                }
            }
        }
        Ok(())
    }

    /// Attemps to fill the buffer with at least `amount` bytes after the offset
    /// `from`.
    ///
    /// If the buffer does not have enough capacity it is replaced with a new
    /// one, and `from` is reset to the corresponding offset in the new buffer.
    pub fn fill_or_replace<R>(&mut self,
                              read: &mut R,
                              from: &mut usize,
                              amount: usize)
                              -> io::Result<()>
    where R: io::Read {
        assert!(*from <= self.offset);
        let buffered_amount = self.offset - *from;
        if buffered_amount >= amount {
            return Ok(());
        }
        let remaining_amount = amount - buffered_amount;

        if remaining_amount > self.raw.len() - self.offset {

            // Replace self with a new buffer with sufficient capacity. Copy
            // over all bytes between `from` and the current write offset, and
            // reset `from` to 0.
            let old_buf = mem::replace(self, MutBuf::with_capacity(cmp::max(BUF_SIZE, amount + 8)));
            try!(self.write(&old_buf[*from..]));
            *from = 0;
        }

        self.fill(read, remaining_amount)
    }
}

impl ops::Deref for MutBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(self.raw.buf(), self.offset)
        }
    }
}

/// A view into a `MutBuf`.
///
/// A `Buf` increments the reference count of the `MutBuf`, so that a `Buf` can
/// outlive the `MutBuf` from which it was created.
///
/// The reference counting mechanism of `MutBuf` is not threadsafe, so `Buf`
/// instances may not be shared or sent across thread boundaries.
pub struct Buf {
    raw: RawBuf,
    ptr: *const u8,
    len: usize,
}

impl ops::Deref for Buf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(self.ptr, self.len)
        }
    }
}

impl Clone for Buf {
    fn clone(&self) -> Buf {
        Buf {
            raw: self.raw.clone(),
            ptr: self.ptr,
            len: self.len
        }
    }
}

/// A reference counted byte buffer.
///
/// The reference count is the first 8 bytes of the buffer.
/// The buffer is not initialized.
///
/// It is left to the user to ensure that data races do not occur and
/// unitialized data is not read.
///
/// `RawBuf` is not threadsafe, and may not be sent or shared across thread
/// boundaries.
struct RawBuf {
    bytes: *mut u8,
    len: usize,
}

impl RawBuf {
    /// Creates a new `RawBuf` instance with approximately the provided
    /// length.
    fn new(len: usize) -> RawBuf {
        unsafe {
            let refcount_len = mem::size_of::<u64>();
            let len = cmp::max(refcount_len, len);
            // The buffer is aligned to a u64. This is necessary for storing the
            // refcount, as well as required by Cap'n Proto. This requirement is
            // the primary reason that the raw allocation APIs are used instead
            // of something like RawVec.
            let bytes = heap::allocate(len, refcount_len);
            if bytes == ptr::null_mut() { alloc::oom() }
            *(bytes as *mut u64) = 1;
            RawBuf {
                bytes: bytes.offset(refcount_len as isize),
                len: len - refcount_len,
            }
        }
    }

    fn buf(&self) -> *mut u8 {
        self.bytes
    }

    fn len(&self) -> usize {
        self.len
    }
}

impl Clone for RawBuf {
    fn clone(&self) -> RawBuf {
        unsafe {
            *(self.bytes.offset(-(mem::size_of::<u64>() as isize)) as *mut u64) += 1;
            RawBuf {
                bytes: self.bytes,
                len: self.len,
            }
        }
    }
}

impl Drop for RawBuf {
    fn drop(&mut self) {
        unsafe {
            let refcount_len = mem::size_of::<u64>();
            let allocation = self.bytes.offset(-(refcount_len as isize));
            let refcount = allocation as *mut u64;
            *refcount -= 1;
            if *refcount == 0 {
                heap::deallocate(allocation, self.len + refcount_len, refcount_len);
            }
        }
    }
}

#[cfg(test)]
mod test {

    use std::io::{Cursor, Write};

    use super::{MutBuf, RawBuf};

    use quickcheck::{quickcheck, TestResult};

    #[test]
    fn test_create_raw_buf() {
        let raw = RawBuf::new(128 * 1024);
        assert_eq!(128 * 1024 - 8, raw.len());
    }

    #[test]
    fn raw_buf_is_cloneable() {
        let raw = RawBuf::new(0);
        let clone = raw.clone();
        assert_eq!(0, clone.len());
    }

    #[test]
    fn mut_buf_write() {
        let mut buf = MutBuf::with_capacity(16);
        assert_eq!(8, buf.write(b"abcdefghijk").unwrap());
        assert_eq!(0, buf.write(b"abcdefghijk").unwrap());
    }

    #[test]
    fn buf() {
        let mut buf = MutBuf::with_capacity(16);
        buf.write_all(b"abcdefgh").unwrap();
        assert_eq!(b"", &*buf.buf(0, 0));
        assert_eq!(b"a", &*buf.buf(0, 1));
        assert_eq!(b"ab", &*buf.buf(0, 2));
        assert_eq!(b"abc", &*buf.buf(0, 3));
        assert_eq!(b"abcd", &*buf.buf(0, 4));
        assert_eq!(b"abcde", &*buf.buf(0, 5));
        assert_eq!(b"abcdef", &*buf.buf(0, 6));
        assert_eq!(b"abcdefg", &*buf.buf(0, 7));
        assert_eq!(b"abcdefgh", &*buf.buf(0, 8));
    }

    #[test]
    fn fill_or_replace() {
        let mut buf = MutBuf::with_capacity(14);
        buf.write_all(b"abcdef").unwrap();
        let mut offset = 3;
        buf.fill_or_replace(&mut Cursor::new("ghi"), &mut offset, 6).unwrap();
        assert_eq!(b"defghi", &*buf.buf(offset, 6));
    }

    #[test]
    fn check_buf() {
        fn buf(segments: Vec<Vec<u8>>) -> TestResult {
            let total_len: usize = segments.iter().fold(0, |acc, segment| acc + segment.len());
            let mut buf = MutBuf::with_capacity(total_len + 8);

            for segment in &segments {
                buf.write_all(&*segment).unwrap();
            }

            let mut offset = 0;
            for segment in &segments {
                if &segment[..] != &*buf.buf(offset, segment.len()) {
                    return TestResult::failed();
                }
                assert_eq!(&segment[..], &*buf.buf(offset, segment.len()));
                offset += segment.len();
            }

            TestResult::passed()
        }

        quickcheck(buf as fn(Vec<Vec<u8>>) -> TestResult);
    }

    #[test]
    fn check_fill() {
        fn fill(segments: Vec<Vec<u8>>) -> TestResult {
            let total_len: usize = segments.iter().fold(0, |acc, segment| acc + segment.len());
            let mut buf = MutBuf::with_capacity(total_len + 8);

            for segment in &segments {
                buf.fill(&mut Cursor::new(segment), segment.len()).unwrap();
            }

            let mut offset = 0;
            for segment in &segments {
                if &segment[..] != &*buf.buf(offset, segment.len()) {
                    return TestResult::failed();
                }
                assert_eq!(&segment[..], &*buf.buf(offset, segment.len()));
                offset += segment.len();
            }

            TestResult::passed()
        }

        quickcheck(fill as fn(Vec<Vec<u8>>) -> TestResult);
    }

    #[test]
    fn check_fill_or_replace() {
        fn fill(a: Vec<u8>, b: Vec<u8>, c: Vec<u8>) -> TestResult {
            let mut buf = MutBuf::with_capacity(8 + a.len() + b.len());

            buf.write_all(&a).unwrap();
            buf.write_all(&b).unwrap();

            let mut offset = a.len();

            buf.fill_or_replace(&mut Cursor::new(&c), &mut offset, b.len() + c.len()).unwrap();

            if &b[..] != &*buf.buf(offset, b.len()) {
                return TestResult::failed();
            }

            if &c[..] != &*buf.buf(offset + b.len(), c.len()) {
                return TestResult::failed();
            }

            TestResult::passed()
        }

        quickcheck(fill as fn(Vec<u8>, Vec<u8>, Vec<u8>) -> TestResult);
    }
}
