#![no_main]
//! The reader of HTTP requests (`silt::http_wire`), which `http.serve`
//! hands whatever a client sends: on any bytes it must end without a
//! panic, give no request beyond the server's limits, and read the
//! same requests however the bytes arrive.
use libfuzzer_sys::fuzz_target;
use silt::http_wire::{BODY_MAX, HEADERS_MAX, HEAD_MAX, Next, Reader, Request};
use std::io::Read;

/// `bytes`, read at most `piece` at a time.
struct Pieces<'a> {
    bytes: &'a [u8],
    piece: usize,
}

impl Read for Pieces<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.bytes.len().min(self.piece).min(buf.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

/// The requests of `bytes`, and the status of the refusal that ended
/// the reading, if one did.
fn read(bytes: &[u8], piece: usize) -> (Vec<Request>, Option<u16>) {
    let mut reader = Reader::new(Pieces { bytes, piece });
    let mut requests = Vec::new();
    loop {
        match reader.next(&mut |_| Ok(())) {
            Next::Request(request) => {
                assert!(request.body.len() <= BODY_MAX);
                assert!(request.headers.len() <= HEADERS_MAX);
                assert!(!request.method.is_empty() && !request.target.is_empty());
                // Nothing of the head holds a line end.
                let head = request.headers.iter().flat_map(|(name, value)| [name, value]);
                let mut length = 0;
                for text in [&request.method, &request.target].into_iter().chain(head) {
                    assert!(!text.contains(['\r', '\n']), "{text:?}");
                    length += text.len();
                }
                assert!(length <= HEAD_MAX);
                requests.push(request);
                // Every request takes bytes: the reading ends.
                assert!(requests.len() <= bytes.len());
            }
            Next::Refused(refused) => {
                assert!((400..500).contains(&refused.status));
                return (requests, Some(refused.status));
            }
            Next::End => return (requests, None),
            Next::Broken(e) => panic!("a reader of bytes in memory broke: {e}"),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let whole = read(data, usize::MAX);
    for piece in [1, 7] {
        assert!(read(data, piece) == whole, "read differently {piece} bytes at a time");
    }
});
