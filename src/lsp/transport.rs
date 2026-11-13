//! The stdio transport of `silt lsp`: the messages of the editor read
//! from stdin, and the server's written to stdout, each framed with a
//! `Content-Length` header.
//!
//! lsp-server's own transport ends the session on the first message it
//! cannot read: a body that is not JSON, a length that is no number. Here
//! such a message is answered with a JSON-RPC error and the session goes
//! on: -32700 (parse error) for a body that is not JSON or a header with
//! no length that can be read, and for a body over [`MAX_BODY`], whose
//! bytes are skipped unread after the answer; -32600 (invalid request)
//! for JSON that is no message, also a request whose id lsp-server cannot
//! hold (a number that is no 32-bit integer). Only the end of stdin, or an
//! error reading it, ends it.

use std::io::{self, BufRead, Read, Write};
use std::thread;

use lsp_server::{Connection, Message};
use serde_json::{Value, json};

/// The largest message body read: a document is sent whole on each
/// edit, and no source file comes near this.
pub(crate) const MAX_BODY: u64 = 256 * 1024 * 1024;

/// The longest header line read; a longer one is no header.
const MAX_HEADER_LINE: u64 = 8 * 1024;

/// JSON-RPC: the body is not JSON.
const PARSE_ERROR: i64 = -32700;

/// JSON-RPC: the body is JSON, but no request, response or
/// notification.
const INVALID_REQUEST: i64 = -32600;

/// What was read from the editor.
#[derive(Debug)]
pub(crate) enum Frame {
    /// A message.
    Message(Message),
    /// A message that could not be read, to be answered with the error
    /// `code`; `id` is its request's id, when it could be read
    /// (otherwise `null`).
    Fault {
        id: Value,
        code: i64,
        message: String,
        /// The bytes of the input to pass over after the answer: the
        /// body of a message over the limit, which is answered before
        /// it arrives (see [`pass_over`]).
        skip: u64,
    },
    /// The end of the input.
    End,
}

/// Read the next frame from `input`. `Err` only when reading fails.
pub(crate) fn read_frame(input: &mut impl BufRead) -> io::Result<Frame> {
    read_frame_within(input, MAX_BODY)
}

/// [`read_frame`] with `max_body` as the largest body read.
fn read_frame_within(input: &mut impl BufRead, max_body: u64) -> io::Result<Frame> {
    // The headers: lines up to an empty one. An empty line before any
    // header is skipped.
    let mut length: Option<Option<u64>> = None;
    let mut headers = 0;
    loop {
        let mut line = Vec::new();
        let read = input
            .by_ref()
            .take(MAX_HEADER_LINE)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(Frame::End);
        }
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if headers == 0 {
                continue;
            }
            break;
        }
        headers += 1;
        if let Some(value) = content_length(line) {
            length = Some(value.trim().parse::<u64>().ok());
        }
    }
    let length = match length {
        Some(Some(length)) => length,
        Some(None) => return Ok(fault(PARSE_ERROR, "the Content-Length header is no number")),
        None => {
            return Ok(fault(
                PARSE_ERROR,
                "the message has no Content-Length header",
            ));
        }
    };
    if length > max_body {
        // Answered at once, then skipped unread, so the next message is
        // read from its start. (A length near 2^64 is not waited for.)
        return Ok(Frame::Fault {
            id: Value::Null,
            code: PARSE_ERROR,
            message: format!("a message of {length} bytes is over the limit of {max_body} bytes"),
            skip: length,
        });
    }
    let mut body = Vec::new();
    let read = input.by_ref().take(length).read_to_end(&mut body)?;
    if (read as u64) < length {
        return Ok(Frame::End);
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => return Ok(fault(PARSE_ERROR, &format!("the message is not JSON: {e}"))),
    };
    let id = match value.get("id") {
        Some(id @ (Value::Number(_) | Value::String(_))) => id.clone(),
        _ => Value::Null,
    };
    // lsp-server reads an object with an id as a response and one with a
    // method as a notification, whatever else they hold: a request whose
    // method is no string, or whose id is no number or string, would go
    // unanswered.
    if let Some(method) = value.get("method") {
        // (lsp-server holds an id that is a 32-bit integer or a string;
        // a request with another number, `1.5`, would be read as a
        // notification and go unanswered.)
        let id_ok = value.get("id").is_none_or(|id| match id {
            Value::Number(n) => n.as_i64().is_some_and(|n| i32::try_from(n).is_ok()),
            Value::String(_) => true,
            _ => false,
        });
        if !method.is_string() || !id_ok {
            return Ok(Frame::Fault {
                id,
                code: INVALID_REQUEST,
                message: "the message is no request or notification: its method must be a \
                          string, and its id a whole number of 32 bits or a string"
                    .to_string(),
                skip: 0,
            });
        }
    }
    match serde_json::from_value::<Message>(value) {
        Ok(message) => Ok(Frame::Message(message)),
        Err(e) => Ok(Frame::Fault {
            id,
            code: INVALID_REQUEST,
            message: format!("the message is no request, response or notification: {e}"),
            skip: 0,
        }),
    }
}

/// The value of the `Content-Length` header in the header line `line`:
/// its last one, also where it does not start the line. After a message
/// whose length was wrong, the rest of its body is read as a header line,
/// and the next message's header is at its end: it is taken, so the
/// reader finds the messages again. A header whose name only ends in it
/// (`X-Content-Length`) is another header: the name is not behind a
/// letter, a digit, `-` or `_`.
fn content_length(line: &str) -> Option<&str> {
    const NAME: &str = "content-length:";
    let lower = line.to_ascii_lowercase();
    let at = lower.rmatch_indices(NAME).map(|(at, _)| at).find(|at| {
        let before = lower.as_bytes()[..*at].last();
        before.is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_'))
    })?;
    Some(&line[at + NAME.len()..])
}

/// Pass over `bytes` bytes of `input`: whether there were that many.
pub(crate) fn pass_over(input: &mut impl BufRead, bytes: u64) -> io::Result<bool> {
    Ok(io::copy(&mut input.by_ref().take(bytes), &mut io::sink())? == bytes)
}

fn fault(code: i64, message: &str) -> Frame {
    Frame::Fault {
        id: Value::Null,
        code,
        message: message.to_string(),
        skip: 0,
    }
}

/// The JSON-RPC error response for a fault, framed.
fn fault_response(id: Value, code: i64, message: &str) -> Vec<u8> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
    .to_string();
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(body.as_bytes());
    framed
}

/// The threads of the transport, to be joined when the server is done.
pub(crate) struct IoThreads {
    reader: thread::JoinHandle<io::Result<()>>,
    writer: thread::JoinHandle<io::Result<()>>,
}

impl IoThreads {
    /// Wait for both threads: the reader ends after the `exit`
    /// notification or at the end of stdin, the writer once the server's
    /// connection is dropped.
    pub(crate) fn join(self) -> io::Result<()> {
        let reader = self
            .reader
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        let writer = self
            .writer
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        reader.and(writer)
    }
}

/// The server's connection over stdin and stdout.
pub(crate) fn stdio() -> (Connection, IoThreads) {
    let (server, transport) = Connection::memory();
    let Connection {
        sender: to_server,
        receiver: from_server,
    } = transport;
    let writer = thread::Builder::new()
        .name("silt-lsp-writer".to_string())
        .spawn(move || {
            for message in from_server {
                // (The lock is held for the whole message: a fault the
                // reader answers is never written into the middle of it.)
                message.write(&mut io::stdout().lock())?;
            }
            Ok(())
        })
        .expect("spawn the LSP writer thread");
    let reader = thread::Builder::new()
        .name("silt-lsp-reader".to_string())
        .spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                match read_frame(&mut input)? {
                    Frame::Message(message) => {
                        let exit =
                            matches!(&message, Message::Notification(n) if n.method == "exit");
                        if to_server.send(message).is_err() || exit {
                            return Ok(());
                        }
                    }
                    Frame::Fault {
                        id,
                        code,
                        message,
                        skip,
                    } => {
                        eprintln!("silt-lsp: a message could not be read: {message}");
                        {
                            let mut out = io::stdout().lock();
                            out.write_all(&fault_response(id, code, &message))?;
                            out.flush()?;
                        }
                        if !pass_over(&mut input, skip)? {
                            return Ok(());
                        }
                    }
                    Frame::End => return Ok(()),
                }
            }
        })
        .expect("spawn the LSP reader thread");
    (server, IoThreads { reader, writer })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    fn frames(input: &str) -> Vec<Frame> {
        let mut input = io::BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        loop {
            match read_frame(&mut input).unwrap() {
                Frame::End => return out,
                frame => {
                    let skip = match &frame {
                        Frame::Fault { skip, .. } => *skip,
                        _ => 0,
                    };
                    out.push(frame);
                    if !pass_over(&mut input, skip).unwrap() {
                        return out;
                    }
                }
            }
        }
    }

    fn code(frame: &Frame) -> Option<i64> {
        match frame {
            Frame::Fault { code, .. } => Some(*code),
            _ => None,
        }
    }

    const PING: &str = r#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#;

    #[test]
    fn a_message_that_cannot_be_read_is_a_fault_and_the_next_is_read() {
        let input = [
            framed("{bad}"),
            "Content-Length: 99999999999999999999\r\n\r\n".to_string(),
            framed("42"),
            "X-Other: 1\r\n\r\n".to_string(),
            framed(PING),
        ]
        .concat();
        let found = frames(&input);
        assert_eq!(found.len(), 5, "{found:?}");
        assert_eq!(code(&found[0]), Some(PARSE_ERROR));
        assert_eq!(code(&found[1]), Some(PARSE_ERROR));
        assert_eq!(code(&found[2]), Some(INVALID_REQUEST));
        assert_eq!(code(&found[3]), Some(PARSE_ERROR));
        assert!(matches!(&found[4], Frame::Message(Message::Request(r)) if r.method == "shutdown"));
    }

    #[test]
    fn a_body_over_the_limit_is_skipped_unread() {
        let input = format!(
            "Content-Length: 61\r\n\r\n{}{}",
            "x".repeat(61),
            framed(PING)
        );
        let mut reader = io::BufReader::new(input.as_bytes());
        let fault = read_frame_within(&mut reader, 60).unwrap();
        assert_eq!(code(&fault), Some(PARSE_ERROR));
        assert!(matches!(fault, Frame::Fault { skip: 61, .. }), "{fault:?}");
        assert!(pass_over(&mut reader, 61).unwrap());
        assert!(matches!(
            read_frame_within(&mut reader, 60).unwrap(),
            Frame::Message(_)
        ));
    }

    /// A length that is wrong by some bytes either way costs the message
    /// it belongs to (and, when too long, the one after it), never the
    /// rest of the session.
    #[test]
    fn a_wrong_length_loses_no_message_but_its_own_and_the_next() {
        let hover = r#"{"jsonrpc":"2.0","id":9,"method":"textDocument/hover","params":{}}"#;
        let short = format!("Content-Length: {}\r\n\r\n{hover}", hover.len() - 10);
        let found = frames(&[short, framed(PING), framed(PING)].concat());
        assert_eq!(code(&found[0]), Some(PARSE_ERROR), "{found:?}");
        let messages = (found.iter())
            .filter(|f| matches!(f, Frame::Message(_)))
            .count();
        assert_eq!(messages, 2, "{found:?}");
        let long = format!("Content-Length: {}\r\n\r\n{hover}", hover.len() + 10);
        let found = frames(&[long, framed(PING), framed(PING), framed(PING)].concat());
        assert_eq!(code(&found[0]), Some(PARSE_ERROR), "{found:?}");
        let messages = (found.iter())
            .filter(|f| matches!(f, Frame::Message(_)))
            .count();
        assert!(messages >= 1, "{found:?}");
        assert!(matches!(found.last(), Some(Frame::Message(_))), "{found:?}");
    }

    /// A request whose method is no string, or whose id is no number or
    /// string, is answered with -32600, with its id when it has one.
    #[test]
    fn a_malformed_request_is_an_invalid_request() {
        let found = frames(
            &[
                framed(r#"{"jsonrpc":"2.0","id":5,"method":5}"#),
                framed(r#"{"jsonrpc":"2.0","id":"abc","method":[]}"#),
                framed(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"textDocument/hover"}"#),
            ]
            .concat(),
        );
        let ids: Vec<(Option<i64>, Value)> = (found.iter())
            .map(|f| match f {
                Frame::Fault { id, code, .. } => (Some(*code), id.clone()),
                _ => (None, Value::Null),
            })
            .collect();
        assert_eq!(
            ids,
            [
                (Some(INVALID_REQUEST), json!(5)),
                (Some(INVALID_REQUEST), json!("abc")),
                (Some(INVALID_REQUEST), Value::Null),
            ]
        );
    }

    /// A length far over the limit (2^64 - 1) is answered at once, not
    /// after that many bytes came: the answer does not wait for the body.
    #[test]
    fn a_length_over_the_limit_is_answered_before_its_body() {
        let input = format!("Content-Length: {}\r\n\r\n{}", u64::MAX, framed(PING));
        let mut reader = io::BufReader::new(input.as_bytes());
        let fault = read_frame(&mut reader).unwrap();
        assert_eq!(code(&fault), Some(PARSE_ERROR), "{fault:?}");
        assert!(
            matches!(fault, Frame::Fault { skip: u64::MAX, .. }),
            "{fault:?}"
        );
        // Nothing of the body was read yet.
        assert_eq!(reader.fill_buf().unwrap(), framed(PING).as_bytes());
    }

    /// A header whose name only ends in `Content-Length` is another
    /// header, wherever it stands.
    #[test]
    fn a_header_named_like_the_length_is_not_the_length() {
        let input = format!(
            "Content-Length: {}\r\nX-Content-Length: 5\r\n\r\n{PING}",
            PING.len()
        );
        let found = frames(&input);
        assert!(matches!(&found[..], [Frame::Message(_)]), "{found:?}");
        assert_eq!(content_length("}Content-Length: 3"), Some(" 3"));
        assert_eq!(content_length("x-content-length: 3"), None);
        assert_eq!(content_length("a_Content-Length: 4"), None);
        assert_eq!(content_length("{}Content-Length: 4"), Some(" 4"));
    }

    /// A request whose id is a number lsp-server cannot hold (`1.5`, one
    /// over 32 bits) is answered with -32600, not read as a notification.
    #[test]
    fn a_request_with_an_id_that_is_no_32_bit_integer_is_invalid() {
        for id in ["1.5", "4294967296", "-1e3"] {
            let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"textDocument/hover"}}"#);
            let found = frames(&framed(&body));
            assert!(
                matches!(
                    &found[..],
                    [Frame::Fault {
                        code: INVALID_REQUEST,
                        ..
                    }]
                ),
                "{id}: {found:?}"
            );
        }
        let found = frames(&framed(PING));
        assert!(matches!(&found[..], [Frame::Message(_)]), "{found:?}");
    }

    #[test]
    fn input_that_ends_inside_a_message_ends_the_session() {
        assert!(frames("Content-Length: 10\r\n\r\n{\"a\"").is_empty());
        assert!(frames("Content-Length: 10\r\n").is_empty());
    }
}
