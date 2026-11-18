//! URLs: reading, normalising, changing and writing them, by RFC 3986.
//!
//! A [`Url`] is always a valid URL in its normal form: it is made only
//! by [`parse`], [`resolve`] and the `with_` functions, each of which
//! checks and escapes what it is given. Its text is
//! `scheme://[user[:password]@]host[:port]/path[?query][#fragment]`:
//! every Url has the `//` after its scheme, so its path always starts
//! with `/`.
//!
//! What `parse` does, beyond reading the parts:
//!
//! - it writes the scheme and the host in lower case, drops the
//!   scheme's default port (http and ws 80, https and wss 443, ftp 21),
//!   writes the hex digits of an escape in upper case, decodes an escape
//!   of a character that needs none (`%41` is `A`), takes the `.` and
//!   `..` segments out of the path, makes an empty path `/`, and drops an
//!   empty user and password, an empty password, and an empty query or
//!   fragment (a `?` or `#` alone);
//! - it escapes the characters that a URL cannot hold but that have one
//!   reading (a space, every character that is no ASCII, and
//!   `"` `<` `>` `\` `^` `` ` `` `{` `|` `}` `[` `]`, and a `#` inside the
//!   fragment);
//! - it refuses the rest: a control character, a `%` that is no escape,
//!   escapes that are no UTF-8 text, white space at either end, more
//!   than one `@` before the path, and a scheme, host or port that is not
//!   one. A host names its machine plainly: a host that looks like a
//!   number must be a plain dotted IPv4 address, and an IPv6 address
//!   that holds an IPv4 address is refused.
//!
//! An error says what is wrong and the character where, counted from 0
//! as `string.slice` counts. It never holds the text it was given, which
//! may hold a password.

use std::fmt::{self, Write};
use std::net::Ipv6Addr;
use std::ops::Range;

// ── The value ────────────────────────────────────────────────────────

/// A valid URL in its normal form: its text, and where its parts lie in
/// it. Equal, hashed and ordered by its whole text.
#[derive(Clone)]
pub struct Url {
    text: String,
    /// The scheme is `text[..scheme_end]`; `://` follows it.
    scheme_end: usize,
    /// Where the escaped user, password, host, path, query (without its
    /// `?`) and fragment (without its `#`) lie. A part that the URL does
    /// not have is an empty range.
    user: Range<usize>,
    password: Range<usize>,
    host: Range<usize>,
    /// The port, when one is written and it is not the scheme's own.
    port: Option<u16>,
    path: Range<usize>,
    query: Range<usize>,
    fragment: Range<usize>,
}

impl PartialEq for Url {
    fn eq(&self, other: &Url) -> bool {
        self.text == other.text
    }
}

impl Eq for Url {}

impl std::hash::Hash for Url {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl PartialOrd for Url {
    fn partial_cmp(&self, other: &Url) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Url {
    fn cmp(&self, other: &Url) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Url({:?})", self.text)
    }
}

/// The URL with its user and password hidden as `***@`.
impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.user.is_empty() && self.password.is_empty() {
            true => f.write_str(&self.text),
            false => write!(
                f,
                "{}://***@{}",
                self.scheme(),
                &self.text[self.host.start..]
            ),
        }
    }
}

/// Why a text is no URL, or a part cannot be put into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UrlError {
    /// What is wrong, and the character where, counted from 0: where the
    /// fault begins, or the text's length when it ends too early.
    Invalid(String, usize),
    /// A part that a `with_` function was given and the URL cannot hold:
    /// which part, and why.
    InvalidPart(String),
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlError::Invalid(what, at) => write!(f, "{what}, at {at}"),
            UrlError::InvalidPart(what) => f.write_str(what),
        }
    }
}

/// A fault and the byte of the text where it begins.
struct Fault {
    what: String,
    at: usize,
}

impl Fault {
    fn new(what: impl Into<String>, at: usize) -> Fault {
        Fault {
            what: what.into(),
            at,
        }
    }

    /// The error of the fault in `text`, its place counted in
    /// characters.
    fn in_text(self, text: &str) -> UrlError {
        let at = text
            .get(..self.at)
            .map_or(self.at, |head| head.chars().count());
        UrlError::Invalid(self.what, at)
    }
}

/// The parts of a URL, each in its normal, escaped form.
#[derive(Clone, Copy)]
struct Parts<'a> {
    scheme: &'a str,
    user: &'a str,
    password: &'a str,
    host: &'a str,
    port: Option<u16>,
    path: &'a str,
    query: &'a str,
    fragment: &'a str,
}

/// The URL of `parts`, which are in their normal form: a port that is
/// the scheme's own is dropped here.
fn assemble(parts: Parts<'_>) -> Url {
    let mut text = String::with_capacity(
        parts.scheme.len()
            + parts.user.len()
            + parts.password.len()
            + parts.host.len()
            + parts.path.len()
            + parts.query.len()
            + parts.fragment.len()
            + 16,
    );
    let part = |text: &mut String, lead: &str, value: &str| {
        text.push_str(lead);
        let start = text.len();
        text.push_str(value);
        start..text.len()
    };
    text.push_str(parts.scheme);
    let scheme_end = text.len();
    let user = part(&mut text, "://", parts.user);
    let password = match parts.password.is_empty() {
        true => text.len()..text.len(),
        false => part(&mut text, ":", parts.password),
    };
    let credentials = !parts.user.is_empty() || !parts.password.is_empty();
    let host = part(&mut text, if credentials { "@" } else { "" }, parts.host);
    let port = parts
        .port
        .filter(|&port| default_port(parts.scheme) != Some(port));
    if let Some(port) = port {
        let _ = write!(text, ":{port}");
    }
    let path = part(&mut text, "", parts.path);
    let query = match parts.query.is_empty() {
        true => text.len()..text.len(),
        false => part(&mut text, "?", parts.query),
    };
    let fragment = match parts.fragment.is_empty() {
        true => text.len()..text.len(),
        false => part(&mut text, "#", parts.fragment),
    };
    Url {
        text,
        scheme_end,
        user,
        password,
        host,
        port,
        path,
        query,
        fragment,
    }
}

/// The port a scheme has when none is written: only the schemes whose
/// readers agree on one.
pub(crate) fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// Whether a URL of `scheme` must have a host.
fn needs_host(scheme: &str) -> bool {
    default_port(scheme).is_some()
}

// ── Characters ──────────────────────────────────────────────────────

/// RFC 3986's unreserved characters: ALPHA DIGIT `-` `.` `_` `~`.
fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// RFC 3986's sub-delims.
fn sub_delim(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

/// The parts of a URL whose characters are escaped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Userinfo,
    Path,
    Query,
    Fragment,
}

/// Whether a part holds `byte` as it is, in a text being read.
fn kept_raw(part: Part, byte: u8) -> bool {
    unreserved(byte)
        || sub_delim(byte)
        || match part {
            Part::Userinfo => byte == b':',
            Part::Path => matches!(byte, b':' | b'@' | b'/'),
            Part::Query | Part::Fragment => matches!(byte, b':' | b'@' | b'/' | b'?'),
        }
}

/// `byte` as an escape, `%HH` with upper-case hex digits, after `out`.
fn push_escape(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[usize::from(byte >> 4)] as char);
    out.push(HEX[usize::from(byte & 0xF)] as char);
}

/// `text` with every byte that `raw` does not keep written as an escape.
fn escaped(text: &str, raw: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        match raw(byte) {
            true => out.push(byte as char),
            false => push_escape(&mut out, byte),
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

/// The byte of the escape that starts at byte `at` of `text` (a `%`),
/// or what is wrong with it.
fn escape_at(text: &str, at: usize) -> Result<u8, Fault> {
    let after = &text.as_bytes()[at + 1..];
    match after {
        [high, low, ..] => match (hex_value(*high), hex_value(*low)) {
            (Some(high), Some(low)) => Ok(high << 4 | low),
            _ if high.is_ascii_graphic() && low.is_ascii_graphic() => Err(Fault::new(
                format!("`%{}{}` is no escape", *high as char, *low as char),
                at,
            )),
            _ => Err(Fault::new("a `%` without two hex digits is no escape", at)),
        },
        // (One byte left is one ASCII character.)
        _ => Err(Fault::new(
            format!("`{}` at the end is no escape", &text[at..]),
            at,
        )),
    }
}

/// How a character is named in an error: in quotes, or by its code
/// when it cannot be seen.
fn shown(c: char) -> String {
    match c.is_control() || (c.is_whitespace() && c != ' ') {
        true => format!("U+{:04X}", u32::from(c)),
        false => format!("'{c}'"),
    }
}

/// The normal form of the part `part` of a URL, which is
/// `text[range]`: escapes in upper case, an escape of an unreserved
/// character decoded, every character of one reading escaped. Refused:
/// a `%` that is no escape and escapes that are no UTF-8 text. (A piece
/// of the part between two of its ASCII delimiters is UTF-8 exactly when
/// all of the part is, so the part is judged whole.)
fn normalised(text: &str, range: Range<usize>, part: Part) -> Result<String, Fault> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(range.len());
    let mut decoded = Vec::with_capacity(range.len());
    // (Where each escape's byte is in `decoded`, and where the escape is.)
    let mut escapes: Vec<(usize, usize)> = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let byte = bytes[at];
        if byte == b'%' {
            // (No delimiter of a part is a hex digit: an escape never
            // reaches past the part.)
            let value = escape_at(text, at)?;
            match unreserved(value) {
                true => out.push(value as char),
                false => push_escape(&mut out, value),
            }
            escapes.push((decoded.len(), at));
            decoded.push(value);
            at += 3;
        } else if byte.is_ascii_control() {
            return Err(control_at(text, at));
        } else if byte.is_ascii() {
            match kept_raw(part, byte) {
                true => out.push(byte as char),
                false => push_escape(&mut out, byte),
            }
            decoded.push(byte);
            at += 1;
        } else {
            let width = text[at..].chars().next().map_or(1, char::len_utf8);
            for &byte in &bytes[at..at + width] {
                push_escape(&mut out, byte);
                decoded.push(byte);
            }
            at += width;
        }
    }
    if let Err(e) = std::str::from_utf8(&decoded) {
        // (A character given as itself is whole: the first byte that is
        // no text is an escape's.)
        let bad = e.valid_up_to();
        let escape = escapes.binary_search_by_key(&bad, |&(byte, _)| byte);
        return Err(match escape {
            Ok(index) => {
                let at = escapes[index].1;
                let written = text[at..at + 3].to_ascii_uppercase();
                Fault::new(format!("the escape {written} is no UTF-8 text"), at)
            }
            Err(_) => Fault::new("the escapes are no UTF-8 text", range.start),
        });
    }
    Ok(out)
}

fn control_at(text: &str, at: usize) -> Fault {
    let c = text[at..].chars().next().unwrap_or('\0');
    Fault::new(
        format!("a URL cannot hold the control character {}", shown(c)),
        at,
    )
}

/// The faults a whole text can have before its parts are read: white
/// space at either end, and a control character. `what` names the text
/// ("a URL", "a reference").
fn check_text(text: &str, what: &str) -> Result<(), Fault> {
    let spoken = |c: char| match c {
        ' ' => "a space".to_string(),
        c => format!("the space character {}", shown(c)),
    };
    if let Some(first) = text.chars().next()
        && first.is_whitespace()
        && !first.is_control()
    {
        return Err(Fault::new(
            format!("{what} does not start with {}", spoken(first)),
            0,
        ));
    }
    if let Some((at, _)) = text.char_indices().find(|&(_, c)| c.is_control()) {
        return Err(control_at(text, at));
    }
    if let Some((at, last)) = text.char_indices().next_back()
        && last.is_whitespace()
    {
        return Err(Fault::new(
            format!("{what} does not end with {}", spoken(last)),
            at,
        ));
    }
    Ok(())
}

// ── Reading a URL ───────────────────────────────────────────────────

/// The URL that `text` is, in its normal form.
pub(crate) fn parse(text: &str) -> Result<Url, UrlError> {
    parse_text(text).map_err(|fault| fault.in_text(text))
}

const NO_SCHEME: &str = "a URL starts with a scheme, such as `https://`";

fn scheme_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

/// The byte of the `:` that ends the scheme of `text`, or `None` when
/// `text` does not start with a scheme and its `:`.
fn scheme_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let end = bytes.iter().position(|&byte| !scheme_char(byte))?;
    (bytes[end] == b':').then_some(end)
}

fn parse_text(text: &str) -> Result<Url, Fault> {
    check_text(text, "a URL")?;
    let Some(colon) = scheme_colon(text) else {
        // (A character no scheme holds, before a `:` that would end
        // one, is named; any other text has no scheme.)
        let bytes = text.as_bytes();
        let bad = match bytes.first().is_some_and(u8::is_ascii_alphabetic) {
            true => bytes.iter().position(|&byte| !scheme_char(byte)),
            false => None,
        };
        if let Some(bad) = bad {
            let ends = text[bad..].find([':', '/', '?', '#']);
            if ends.is_some_and(|end| text.as_bytes()[bad + end] == b':') {
                let c = text[bad..].chars().next().unwrap_or(' ');
                return Err(Fault::new(
                    format!("a scheme cannot hold {}", shown(c)),
                    bad,
                ));
            }
        }
        return Err(Fault::new(NO_SCHEME, 0));
    };
    let scheme = text[..colon].to_ascii_lowercase();
    if !text[colon + 1..].starts_with("//") {
        return Err(Fault::new("a URL has `//` after its scheme", colon + 1));
    }
    let authority_start = colon + 3;
    let authority_end = text[authority_start..]
        .find(['/', '?', '#'])
        .map_or(text.len(), |at| authority_start + at);
    let authority = &text[authority_start..authority_end];
    let mut ats = authority
        .match_indices('@')
        .map(|(at, _)| authority_start + at);
    let at_sign = ats.next();
    if let Some(second) = ats.next() {
        return Err(Fault::new("a user or password holds `@` as %40", second));
    }
    let (user, password, host_start) = match at_sign {
        None => (String::new(), String::new(), authority_start),
        Some(at_sign) => {
            let colon = text[authority_start..at_sign]
                .find(':')
                .map(|at| authority_start + at);
            let user_end = colon.unwrap_or(at_sign);
            let user = normalised(text, authority_start..user_end, Part::Userinfo)?;
            let password = match colon {
                Some(colon) => normalised(text, colon + 1..at_sign, Part::Userinfo)?,
                None => String::new(),
            };
            (user, password, at_sign + 1)
        }
    };
    let (host, port) = host_and_port(text, host_start..authority_end, &scheme)?;
    let path_end = text[authority_end..]
        .find(['?', '#'])
        .map_or(text.len(), |at| authority_end + at);
    let path = normalised(text, authority_end..path_end, Part::Path)?;
    let path = match path.is_empty() {
        true => "/".to_string(),
        false => without_dot_segments(&path),
    };
    let query_end = text[path_end..]
        .find('#')
        .map_or(text.len(), |at| path_end + at);
    let query = match text[path_end..].starts_with('?') {
        true => normalised(text, path_end + 1..query_end, Part::Query)?,
        false => String::new(),
    };
    let fragment = match query_end < text.len() {
        true => normalised(text, query_end + 1..text.len(), Part::Fragment)?,
        false => String::new(),
    };
    Ok(assemble(Parts {
        scheme: &scheme,
        user: &user,
        password: &password,
        host: &host,
        port,
        path: &path,
        query: &query,
        fragment: &fragment,
    }))
}

/// The host and the port of `text[range]`, the authority after its
/// user and password, for a URL of `scheme`.
fn host_and_port(
    text: &str,
    range: Range<usize>,
    scheme: &str,
) -> Result<(String, Option<u16>), Fault> {
    let written = &text[range.clone()];
    let host_end = match written.starts_with('[') {
        true => match written.find(']') {
            Some(close) => {
                let after = range.start + close + 1;
                if after < range.end && text.as_bytes()[after] != b':' {
                    return Err(Fault::new(
                        "after the `]` of an IPv6 address comes `:` and a port, or the path",
                        after,
                    ));
                }
                after
            }
            None => {
                return Err(Fault::new(
                    "the `[` of an IPv6 address has no `]`",
                    range.start,
                ));
            }
        },
        false => written.find(':').map_or(range.end, |at| range.start + at),
    };
    // (An `@` after the host's start may end a user's token that a raw
    // `/` cut short, `https://tökén/x@h/`: then what reads as the host is
    // the token, and its ASCII form is not named.)
    let named = !text[range.start..].contains('@');
    let host = checked_host(&text[range.start..host_end], scheme, named)
        .map_err(|fault| Fault::new(fault.what, range.start + fault.at))?;
    let port = match host_end < range.end {
        true => Some(port_of(text, host_end + 1..range.end)?),
        false => None,
    };
    Ok((host, port))
}

/// The port that `text[range]` is: digits only, 1 to 65535.
fn port_of(text: &str, range: Range<usize>) -> Result<u16, Fault> {
    let digits = &text[range.clone()];
    if digits.is_empty() {
        return Err(Fault::new("a `:` after the host has no port", range.start));
    }
    if let Some((at, c)) = digits.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        return Err(Fault::new(
            format!("a port cannot hold {}", shown(c)),
            range.start + at,
        ));
    }
    let number = digits.trim_start_matches('0');
    match number.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ if number.len() <= 10 => Err(Fault::new(
            format!(
                "port {} is not in 1..65535",
                if number.is_empty() { "0" } else { number }
            ),
            range.start,
        )),
        _ => Err(Fault::new("a port is a number in 1..65535", range.start)),
    }
}

const NUMBER_LIKE: &str =
    "a host that looks like a number must be an IPv4 address (four numbers 0-255)";

/// The normal form of the host `host` of a URL of `scheme`, or what is
/// wrong with it and at which of its bytes.
fn checked_host(host: &str, scheme: &str, named: bool) -> Result<String, Fault> {
    if host.is_empty() {
        return match needs_host(scheme) {
            true => Err(Fault::new(
                format!("a URL of scheme `{scheme}` has a host"),
                0,
            )),
            false => Ok(String::new()),
        };
    }
    if host.starts_with('[') && host.ends_with(']') {
        return ipv6_host(&host[1..host.len() - 1]).map(|address| format!("[{address}]"));
    }
    for (at, c) in host.char_indices() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            continue;
        }
        if !c.is_ascii() {
            let what = match punycode_host(host).filter(|_| named) {
                Some(ascii) => format!("a host name must be ASCII: write it as {ascii}"),
                None => "a host name must be ASCII: write its punycode form (xn--...)".into(),
            };
            return Err(Fault::new(what, at));
        }
        return Err(Fault::new(format!("a host cannot hold {}", shown(c)), at));
    }
    let host = host.to_ascii_lowercase();
    let name = host.strip_suffix('.').unwrap_or(&host);
    if name.len() > 253 {
        return Err(Fault::new("a host name is at most 253 characters long", 0));
    }
    let mut label_start = 0;
    for label in name.split('.') {
        if label.is_empty() {
            return Err(Fault::new("a host name has no empty label", label_start));
        }
        if label.len() > 63 {
            return Err(Fault::new(
                "a label of a host name is at most 63 characters long",
                label_start,
            ));
        }
        label_start += label.len() + 1;
    }
    let last = name.rsplit('.').next().unwrap_or(name);
    let number_like = last.bytes().all(|byte| byte.is_ascii_digit()) || last.starts_with("0x");
    if number_like {
        if !plain_ipv4(name) {
            return Err(Fault::new(NUMBER_LIKE, 0));
        }
        if name.len() < host.len() {
            return Err(Fault::new("an IPv4 address has no `.` at its end", 0));
        }
    }
    Ok(host)
}

/// Whether `text` is an IPv4 address in its plain form: four numbers
/// 0 to 255 with dots between, without leading zeros.
fn plain_ipv4(text: &str) -> bool {
    let mut parts = 0;
    for part in text.split('.') {
        parts += 1;
        let digits = part.bytes().all(|byte| byte.is_ascii_digit());
        let plain =
            !part.is_empty() && part.len() <= 3 && !(part.len() > 1 && part.starts_with('0'));
        if !(digits && plain && part.parse::<u16>().is_ok_and(|n| n <= 255)) {
            return false;
        }
    }
    parts == 4
}

const HOLDS_IPV4: &str = "an IPv6 address that holds an IPv4 address: write the IPv4 address";

/// The IPv6 address written `inside` brackets, or what is wrong with it
/// (its place being the `[`).
fn ipv6_host(inside: &str) -> Result<Ipv6Addr, Fault> {
    if let Some(at) = inside.find('%') {
        return Err(Fault::new(
            "an IPv6 address with a zone (`%`) is not taken",
            1 + at,
        ));
    }
    let Ok(address) = inside.parse::<Ipv6Addr>() else {
        return Err(Fault::new("the brackets hold no IPv6 address", 0));
    };
    let segments = address.segments();
    let mapped = address.to_ipv4_mapped().is_some();
    let compatible = segments[..6] == [0; 6] && !matches!(segments[6..], [0, 0] | [0, 1]);
    match mapped || compatible {
        true => Err(Fault::new(HOLDS_IPV4, 0)),
        false => Ok(address),
    }
}

// ── Punycode, for the error of a host that is no ASCII ───────────────

/// The ASCII form of the host `host` when it is surely what IDNA makes
/// of it without its tables: every character that is no ASCII is a
/// lower-case letter of Latin-1 Supplement or Latin Extended-A that
/// IDNA keeps as it is (precomposed, so the label is NFC as written),
/// and the ASCII form is a valid host. `None` for any other host.
fn punycode_host(host: &str) -> Option<String> {
    let kept = |c: char| match u32::from(c) {
        0xE0..=0xFE => c != '\u{F7}',
        0x100..=0x17F => c.is_lowercase() && !matches!(c, 'ĳ' | 'ŀ' | 'ŉ' | 'ſ'),
        _ => c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'),
    };
    if !host.chars().all(kept) {
        return None;
    }
    let mut labels = Vec::new();
    for label in host.to_ascii_lowercase().split('.') {
        labels.push(match label.is_ascii() {
            true => label.to_string(),
            false => format!("xn--{}", punycode(label)?),
        });
    }
    let ascii = labels.join(".");
    // (The scheme matters only for an empty host, which this is not.)
    checked_host(&ascii, "", false).ok()
}

/// RFC 3492's encoding of `label`, without the `xn--`.
fn punycode(label: &str) -> Option<String> {
    const BASE: u32 = 36;
    let digit = |d: u32| match d {
        0..=25 => (b'a' + d as u8) as char,
        _ => (b'0' + (d - 26) as u8) as char,
    };
    let adapt = |delta: u32, points: u32, first: bool| {
        let mut delta = if first { delta / 700 } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - 1) * 26) / 2 {
            delta /= BASE - 1;
            k += BASE;
        }
        k + (BASE * delta) / (delta + 38)
    };
    let input: Vec<u32> = label.chars().map(u32::from).collect();
    let mut output: String = label.chars().filter(char::is_ascii).collect();
    let basic = output.len() as u32;
    let mut handled = basic;
    if basic > 0 {
        output.push('-');
    }
    let (mut n, mut delta, mut bias) = (0x80u32, 0u32, 72u32);
    while (handled as usize) < input.len() {
        let m = input.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;
        for &c in &input {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = match k {
                        k if k <= bias => 1,
                        k if k >= bias + 26 => 26,
                        k => k - bias,
                    };
                    if q < t {
                        break;
                    }
                    output.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(output)
}

// ── Paths ───────────────────────────────────────────────────────────

/// The path `path`, which starts with `/`, without its `.` and `..`
/// segments (RFC 3986 5.2.4): a last one leaves the path ending in `/`
/// (`/a/b/..` is `/a/`).
fn without_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path[1..].split('/').collect();
    let mut kept: Vec<&str> = Vec::with_capacity(segments.len());
    let last = segments.len() - 1;
    for (at, segment) in segments.into_iter().enumerate() {
        match segment {
            "." | ".." => {
                if segment == ".." {
                    kept.pop();
                }
                if at == last {
                    kept.push("");
                }
            }
            segment => kept.push(segment),
        }
    }
    let mut out = String::with_capacity(path.len());
    for segment in kept {
        out.push('/');
        out.push_str(segment);
    }
    out
}

/// `text` with its escapes decoded, and each `+` as a space when
/// `plus_is_space`; what is wrong, and at which byte of it, when it has a
/// `%` that is no escape or its bytes are no UTF-8 text.
fn decoded(text: &str, plus_is_space: bool) -> Result<String, Fault> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len());
    let mut escapes: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                escapes.push((out.len(), at));
                out.push(escape_at(text, at)?);
                at += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| {
        let bad = e.utf8_error().valid_up_to();
        match escapes.binary_search_by_key(&bad, |&(byte, _)| byte) {
            Ok(index) => {
                let at = escapes[index].1;
                let written = text[at..at + 3].to_ascii_uppercase();
                Fault::new(format!("the escape {written} is no UTF-8 text"), at)
            }
            Err(_) => Fault::new("the escapes are no UTF-8 text", 0),
        }
    })
}

/// A part of a Url decoded: it holds only valid escapes of UTF-8 text.
fn decoded_part(text: &str, plus_is_space: bool) -> String {
    let got = decoded(text, plus_is_space);
    debug_assert!(got.is_ok(), "a part of a Url does not decode");
    got.unwrap_or_else(|_| text.to_string())
}

// ── The Url's parts ─────────────────────────────────────────────────

impl Url {
    /// The whole text, user and password included.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn scheme(&self) -> &str {
        &self.text[..self.scheme_end]
    }

    /// The user, decoded; "" when there is none.
    pub(crate) fn user(&self) -> String {
        decoded_part(&self.text[self.user.clone()], false)
    }

    /// The password, decoded; "" when there is none.
    pub(crate) fn password(&self) -> String {
        decoded_part(&self.text[self.password.clone()], false)
    }

    /// The host: a name, an IPv4 address, an IPv6 address in brackets,
    /// or "".
    pub(crate) fn host(&self) -> &str {
        &self.text[self.host.clone()]
    }

    /// The port when one is written and it is not the scheme's own.
    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }

    /// The path as it stands in the URL, escaped.
    pub(crate) fn path(&self) -> &str {
        &self.text[self.path.clone()]
    }

    /// The path's segments, decoded: `/` is none, `/a/` is `a` and "".
    pub(crate) fn segments(&self) -> Vec<String> {
        match self.path() {
            "/" => Vec::new(),
            path => path[1..]
                .split('/')
                .map(|segment| decoded_part(segment, false))
                .collect(),
        }
    }

    /// The query as it stands in the URL, escaped, without its `?`; ""
    /// when there is none.
    pub(crate) fn query_text(&self) -> &str {
        &self.text[self.query.clone()]
    }

    /// The query's pairs, decoded by the rules of forms (`+` is a space),
    /// in order: `flag` is `flag` and "".
    pub(crate) fn query(&self) -> Vec<(String, String)> {
        form_pairs(self.query_text())
    }

    /// The fragment, decoded; "" when there is none.
    pub(crate) fn fragment(&self) -> String {
        decoded_part(&self.text[self.fragment.clone()], false)
    }

    fn parts(&self) -> Parts<'_> {
        Parts {
            scheme: self.scheme(),
            user: &self.text[self.user.clone()],
            password: &self.text[self.password.clone()],
            host: self.host(),
            port: self.port,
            path: self.path(),
            query: self.query_text(),
            fragment: &self.text[self.fragment.clone()],
        }
    }

    // ── Changing a Url ──────────────────────────────────────────────

    pub(crate) fn with_scheme(&self, scheme: &str) -> Result<Url, UrlError> {
        let bytes = scheme.as_bytes();
        if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
            return Err(UrlError::InvalidPart(
                "a scheme starts with a letter".into(),
            ));
        }
        if let Some(c) = scheme
            .chars()
            .find(|&c| !c.is_ascii() || !scheme_char(c as u8))
        {
            return Err(UrlError::InvalidPart(format!(
                "a scheme cannot hold {}",
                shown(c)
            )));
        }
        let scheme = scheme.to_ascii_lowercase();
        if self.host().is_empty() && needs_host(&scheme) {
            return Err(UrlError::InvalidPart(format!(
                "a URL of scheme `{scheme}` has a host"
            )));
        }
        Ok(assemble(Parts {
            scheme: &scheme,
            ..self.parts()
        }))
    }

    /// The Url with the user `user`, escaped; "" removes it.
    pub(crate) fn with_user(&self, user: &str) -> Url {
        let user = escaped(user, unreserved);
        assemble(Parts {
            user: &user,
            ..self.parts()
        })
    }

    /// The Url with the password `password`, escaped; "" removes it.
    pub(crate) fn with_password(&self, password: &str) -> Url {
        let password = escaped(password, unreserved);
        assemble(Parts {
            password: &password,
            ..self.parts()
        })
    }

    /// The Url with the host `host`, written as [`Url::host`] gives one.
    pub(crate) fn with_host(&self, host: &str) -> Result<Url, UrlError> {
        let host = checked_host(host, self.scheme(), true)
            .map_err(|fault| UrlError::InvalidPart(fault.what))?;
        Ok(assemble(Parts {
            host: &host,
            ..self.parts()
        }))
    }

    /// The Url with the port `port`: `None`, or the scheme's own, writes
    /// none.
    pub(crate) fn with_port(&self, port: Option<i64>) -> Result<Url, UrlError> {
        let port = match port {
            None => None,
            Some(port) => match u16::try_from(port) {
                Ok(port) if port > 0 => Some(port),
                _ => {
                    return Err(UrlError::InvalidPart(format!(
                        "port {port} is not in 1..65535"
                    )));
                }
            },
        };
        Ok(assemble(Parts {
            port,
            ..self.parts()
        }))
    }

    /// The Url with the path of `segments`, each escaped. A segment `.`
    /// or `..` is refused: it would be read back as a dot segment. No
    /// segments, and one empty one, are the path `/`.
    pub(crate) fn with_segments(&self, segments: &[String]) -> Result<Url, UrlError> {
        let path = segments_path("", segments)?;
        let path = if path.is_empty() { "/" } else { &path };
        Ok(assemble(Parts {
            path,
            ..self.parts()
        }))
    }

    /// The Url with `segments` after its path, each escaped, a last
    /// empty segment of its path (a `/` at its end) dropped first: `/v1`
    /// and `/v1/` with `users` both give `/v1/users`. No segments give
    /// the Url as it is.
    pub(crate) fn add_segments(&self, segments: &[String]) -> Result<Url, UrlError> {
        if segments.is_empty() {
            return Ok(self.clone());
        }
        let base = self.path().strip_suffix('/').unwrap_or(self.path());
        let path = segments_path(base, segments)?;
        let path = if path.is_empty() { "/" } else { &path };
        Ok(assemble(Parts {
            path,
            ..self.parts()
        }))
    }

    /// The Url with the query of `pairs`, each key and value escaped
    /// (a space as `%20`); no pairs remove the query.
    pub(crate) fn with_query(&self, pairs: &[(String, String)]) -> Url {
        let raw = |byte: u8| {
            unreserved(byte)
                || matches!(byte, b'!' | b'$' | b'\'' | b'(' | b')' | b'*' | b',')
                || matches!(byte, b':' | b'@' | b'/' | b'?')
        };
        let mut query = String::new();
        for (at, (key, value)) in pairs.iter().enumerate() {
            if at > 0 {
                query.push('&');
            }
            query.push_str(&escaped(key, raw));
            query.push('=');
            query.push_str(&escaped(value, raw));
        }
        assemble(Parts {
            query: &query,
            ..self.parts()
        })
    }

    /// The Url with the fragment `fragment`, escaped; "" removes it.
    pub(crate) fn with_fragment(&self, fragment: &str) -> Url {
        let raw = |byte| kept_raw(Part::Fragment, byte);
        let fragment = escaped(fragment, raw);
        assemble(Parts {
            fragment: &fragment,
            ..self.parts()
        })
    }
}

/// `base` (a path without a `/` at its end, or "") with a `/` and each
/// of `segments`, escaped.
fn segments_path(base: &str, segments: &[String]) -> Result<String, UrlError> {
    let raw = |byte| unreserved(byte) || sub_delim(byte) || matches!(byte, b':' | b'@');
    let mut path = base.to_string();
    for segment in segments {
        if matches!(segment.as_str(), "." | "..") {
            return Err(UrlError::InvalidPart(format!(
                "a segment cannot be `{segment}`"
            )));
        }
        path.push('/');
        path.push_str(&escaped(segment, raw));
    }
    Ok(path)
}

// ── Resolving a reference ───────────────────────────────────────────

/// The URL that `reference` names when it is read on the page `base`
/// (RFC 3986 5.2): a whole URL, `//host/x` on the base's scheme, `/x`,
/// a path next to the base's (`x`, `../x`), `?q`, `#f`, or "" (the base
/// without its fragment). A reference with a scheme but no `//` (`a:b`,
/// `http:g`) is refused, as [`parse`] refuses it.
pub(crate) fn resolve(base: &Url, reference: &str) -> Result<Url, UrlError> {
    resolve_text(base, reference).map_err(|fault| fault.in_text(reference))
}

fn resolve_text(base: &Url, reference: &str) -> Result<Url, Fault> {
    check_text(reference, "a reference")?;
    if scheme_colon(reference).is_some() {
        return parse_text(reference);
    }
    if reference.starts_with("//") {
        let lead = base.scheme_end + 1;
        let whole = format!("{}:{reference}", base.scheme());
        // (The scheme and its `:` are ASCII: a byte of `whole` after
        // them is that byte of the reference.)
        return parse_text(&whole)
            .map_err(|fault| Fault::new(fault.what, fault.at.saturating_sub(lead)));
    }
    let query_end = reference.find('#').unwrap_or(reference.len());
    let path_end = reference[..query_end].find('?').unwrap_or(query_end);
    let path = normalised(reference, 0..path_end, Part::Path)?;
    let query = match path_end < query_end {
        true => Some(normalised(reference, path_end + 1..query_end, Part::Query)?),
        false => None,
    };
    let fragment = match query_end < reference.len() {
        true => normalised(reference, query_end + 1..reference.len(), Part::Fragment)?,
        false => String::new(),
    };
    let base_parts = base.parts();
    let (path, query) = match path.is_empty() {
        true => (
            base.path().to_string(),
            query.unwrap_or_else(|| base.query_text().to_string()),
        ),
        false => {
            let path = match path.starts_with('/') {
                true => path,
                false => {
                    let own = base.path();
                    format!("{}{path}", &own[..own.rfind('/').map_or(0, |at| at + 1)])
                }
            };
            (without_dot_segments(&path), query.unwrap_or_default())
        }
    };
    Ok(assemble(Parts {
        path: &path,
        query: &query,
        fragment: &fragment,
        ..base_parts
    }))
}

// ── Escaping text ───────────────────────────────────────────────────

/// `text` with every byte but the unreserved characters escaped, for
/// one piece of a URL.
pub(crate) fn escape(text: &str) -> String {
    escaped(text, unreserved)
}

/// `text` with its escapes decoded; refused when it has a `%` that is
/// no escape, or when its bytes are no UTF-8 text.
pub(crate) fn unescape(text: &str) -> Result<String, UrlError> {
    decoded(text, false).map_err(|fault| fault.in_text(text))
}

/// The form body (`application/x-www-form-urlencoded`) of `pairs`:
/// `key=value` joined by `&`, a space written `+`, every byte but
/// letters, digits and `*` `-` `.` `_` escaped.
pub(crate) fn encode_form(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    let component = |out: &mut String, text: &str| {
        for &byte in text.as_bytes() {
            match byte {
                b' ' => out.push('+'),
                byte if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'*' | b'-' | b'.' | b'_') =>
                {
                    out.push(byte as char)
                }
                byte => push_escape(out, byte),
            }
        }
    };
    for (at, (key, value)) in pairs.iter().enumerate() {
        if at > 0 {
            out.push('&');
        }
        component(&mut out, key);
        out.push('=');
        component(&mut out, value);
    }
    out
}

/// The pairs of the form body `body`: split on `&` (an empty piece is
/// skipped), each on its first `=` (none is an empty value), `+` read as
/// a space and escapes decoded.
pub(crate) fn decode_form(body: &str) -> Result<Vec<(String, String)>, UrlError> {
    let mut pairs = Vec::new();
    let mut start = 0;
    for piece in body.split('&') {
        let piece_start = start;
        start += piece.len() + 1;
        if piece.is_empty() {
            continue;
        }
        let (key, value) = piece.split_once('=').unwrap_or((piece, ""));
        let value_start = piece_start + key.len() + 1;
        let key = decoded(key, true).map_err(|f| Fault::new(f.what, piece_start + f.at));
        let value = decoded(value, true).map_err(|f| Fault::new(f.what, value_start + f.at));
        match (key, value) {
            (Ok(key), Ok(value)) => pairs.push((key, value)),
            (Err(fault), _) | (_, Err(fault)) => return Err(fault.in_text(body)),
        }
    }
    Ok(pairs)
}

/// The pairs of a query of a Url, which decodes.
fn form_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let (key, value) = piece.split_once('=').unwrap_or((piece, ""));
            (decoded_part(key, true), decoded_part(value, true))
        })
        .collect()
}

#[cfg(test)]
mod tests;
