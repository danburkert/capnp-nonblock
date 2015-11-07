#![feature(alloc, heap_api, oom)]

extern crate alloc;
#[macro_use]
extern crate nom;

mod buf;

use nom::le_u32;

/// Parses a segment table into a sequence of segment lengths, in words.
///
/// Fails if the number of segments in the table is invalid.
named!(parse_segment_table<Vec<u32> >,
       chain!(
           segment_count: map!(le_u32, |count| (count as usize).wrapping_add(1)) ~
           segment_lengths: cond_reduce!(segment_count == 0 || segment_count < 512,
                                         count!(le_u32, segment_count as usize)),
           || { segment_lengths }));

/// Given the number of segments of a message, returns the length of the segment
/// table in bytes.
fn segment_table_length(segment_count: usize) -> usize {
    segment_count * 4 + if segment_count % 2 == 0 { 8 } else { 4 }
}

#[cfg(test)]
pub mod test {

    use super::{
        parse_segment_table,
        segment_table_length,
    };

    use nom::IResult;

    fn unwrap<I, O, E>(result: IResult<I, O, E>) -> O {
        match result {
            IResult::Done(_, o) => o,
            IResult::Error(..) => panic!("attempted to unwrap an error IResult"),
            IResult::Incomplete(..) => panic!("attempted to unwrap an incomplete IResult: {}"),
        }
    }

    #[test]
    fn test_parse_segment_table() {
        fn compare(expected: &[u32], buf: &[u8]) {
            assert_eq!(expected, &unwrap(parse_segment_table(&buf[..]))[..]);
        }

        compare(&[0], &[0,0,0,0,   // 1 segments
                        0,0,0,0]); // 0 length

        compare(&[1], &[0,0,0,0,   // 1 segments
                        1,0,0,0]); // 0 length

        compare(&[1, 1],
                &[1,0,0,0,   // 2 segments
                  1,0,0,0,   // 1 length
                  1,0,0,0,   // 1 length
                  0,0,0,0]); // padding

        compare(&[1, 1, 256],
                &[2,0,0,0,   // 3 segments
                  1,0,0,0,   // 1 length
                  1,0,0,0,   // 1 length
                  0,1,0,0]); // 256 length

        compare(&[77, 23, 1, 99],
                &[3,0,0,0,
                  77,0,0,0,   // 77 length
                  23,0,0,0,   // 23 length
                  1,0,0,0,    // 1 length
                  99,0,0,0]); // 99 length
    }

    #[test]
    fn test_parse_invalid_segment_table() {
        assert!(parse_segment_table(&[255,1,0,0]).is_err());
        assert!(parse_segment_table(&[0,0,0,0]).is_incomplete());
        assert!(parse_segment_table(&[0,0,0,0, 0,0,0]).is_incomplete());
        assert!(parse_segment_table(&[1,0,0,0, 0,0,0,0, 0,0,0]).is_incomplete());
        assert!(parse_segment_table(&[255,255,255,255]).is_err());
    }

    #[test]
    fn test_segment_table_length() {
        assert_eq!(8, segment_table_length(1));
        assert_eq!(16, segment_table_length(2));
        assert_eq!(16, segment_table_length(3));
        assert_eq!(24, segment_table_length(4));
        assert_eq!(24, segment_table_length(4));
    }
}
