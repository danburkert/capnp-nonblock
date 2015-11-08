use alloc::{self, heap};
use std::{cmp, io, mem, ops, ptr, slice};

use std::io::Write;

/// Default buffer size. Perhaps this should be tunable.
const BUF_SIZE: usize = 4096;

/// An append only byte buffer.
///
/// `MutBuf` keeps an internal byte buffer to which it allows bytes to be
/// written. Once a byte in the buffer is written, it may never be overwritten,
/// or otherwise recycled.
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
            if offset + len > self.offset {
                panic!("buf out of bounds; requested offset: {}, \
                        requested length: {}, total length: {}",
                       offset, len, self.offset);
            }
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
        let mut buf = unsafe {
            assert!(self.raw.len() - self.offset >= amount);
            slice::from_raw_parts_mut(self.raw.buf().offset(self.offset as isize), amount)
        };

        while !buf.is_empty() {
            match try!(read.read(&mut buf)) {
                0 => return Result::Err(io::Error::new(io::ErrorKind::UnexpectedEOF,
                                                       "failed to fill whole buffer")),
                n => { let mut tmp = buf; buf = &mut tmp[n..] },
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
        assert!(*from < self.offset);

        if self.offset - *from >= amount { return Ok(()) }

        if self.raw.len() - *from < amount {
            // Replace self with a new buffer with sufficient capacity. Copy
            // over all bytes between `from` and the current write offset, and
            // reset `from` to 0.
            let old_buf = mem::replace(self, MutBuf::with_capacity(cmp::max(BUF_SIZE, amount)));
            try!(self.write(&old_buf[*from..]));
            *from = 0;
        }

        let offset = self.offset;
        self.fill(read, amount.wrapping_add(*from).wrapping_sub(offset))
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
/// The user must coordinate among clones to ensure that data races do not
/// occur.
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

    use std::io::Write;
    use super::{MutBuf, RawBuf};

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
}
