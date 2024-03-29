use std::ops::Range;

use hyper::header::{self, ByteRangeSpec};
use std::cmp;

#[derive(Debug, PartialEq)]
pub enum RequestedRange {
    // Client did not provide a range
    None,

    // Client provided a range but it was invalid
    NotSatisfiable,

    // We can serve the client's requested range
    Satisfiable(Range<u64>),
}

pub fn parse_range_header(
    has_header: bool,
    header_value: Option<&header::Range>,
    file_len: u64,
) -> RequestedRange {
    match header_value {
        Some(&header::Range::Bytes(ref byte_ranges)) => {
            // Get the first byte range or short-circuit if none given.
            let byte_range = match byte_ranges.first() {
                Some(range) =>
                    range,
                None =>
                    return RequestedRange::NotSatisfiable,
            };

            // Avoid overflow on zero-length file by short-circuiting if client tries
            // to define a range at all since even 0-0 is impossible.
            if file_len == 0 {
                return RequestedRange::NotSatisfiable;
            }

            let max_end = file_len - 1;

            let range = match *byte_range {
                ByteRangeSpec::FromTo(start, end) => start..(cmp::min(max_end, end)),
                ByteRangeSpec::AllFrom(start) => start..max_end,
                ByteRangeSpec::Last(suffix_len) => {
                    if suffix_len == 0 {
                        return RequestedRange::NotSatisfiable;
                    }
                    // Ensure start cannot be negative
                    let start = max_end - cmp::min(max_end, suffix_len + 1);
                    start..max_end
                }
            };

            // VALIDATION

            // Bad range: start > end
            // FIXME: This doesn't actually check anything because header_value goes to None branch if start > end.
            // I'd prefer to respond NotSatisfiable.
            if range.start > range.end {
                return RequestedRange::NotSatisfiable;
            }

            // Bad range: start >= resource length
            // BAD: fileLength=10 and range is "10-"
            // BAD: fileLength=10 and range is "10-10"
            // GOOD: fileLength=10 and range is "9-"
            if range.start > max_end {
                return RequestedRange::NotSatisfiable;
            }

            RequestedRange::Satisfiable(range)
        }
        // We only support byte ranges.
        Some(_) => RequestedRange::NotSatisfiable,
        None => {
            // req.headers().get(header::Range) will be None if start > end, so we need to
            // check req.headers().has(header::Range) as well to differentiate between
            // missing header and invalid header.
            if has_header {
                RequestedRange::NotSatisfiable
            } else {
                RequestedRange::None
            }
        }
    }
}

#[test]
// TODO: Add more tests. Test failures. Consider using RangeInclusive.
fn test_parse_range_header() {
    // Range given but hyper can't parse it.
    assert_eq!(
        RequestedRange::NotSatisfiable,
        parse_range_header(true, None, 0)
    );
    // Range not given at all
    assert_eq!(RequestedRange::None, parse_range_header(false, None, 0));

    // Zero-length entity cannot be satisfied
    assert_eq!(
        RequestedRange::NotSatisfiable,
        parse_range_header(
            true,
            Some(&header::Range::Bytes(vec![
                header::ByteRangeSpec::FromTo(0, 0),
            ])),
            0
        )
    );

    assert_eq!(
        RequestedRange::Satisfiable(0..0),
        parse_range_header(
            true,
            Some(&header::Range::Bytes(vec![
                header::ByteRangeSpec::FromTo(0, 0),
            ])),
            1
        )
    );
}
