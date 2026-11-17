//! The `url.*` builtin functions, on the reader and writer of URLs in
//! `src/url.rs`.

use std::sync::Arc;

use super::common::{ok, too_long_text};
use super::typed::{self, List, builtins, unsound};
use crate::typeinfo::bv;
use crate::url::{self, Url, UrlError};
use crate::value::{MAX_TEXT_LEN, Value};
use crate::vm::VmError;

/// How `UrlError` shows the variant `tag` with `fields`: `None` if they
/// are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    match (tag, fields) {
        ("Invalid", [Value::String(what, _), Value::Int(at)]) => Some(format!("{what}, at {at}")),
        ("InvalidPart", [Value::String(what, _)]) => Some(what.to_string()),
        _ => None,
    }
}

/// The `UrlError` value of `error`.
pub(crate) fn error_value(error: UrlError) -> Value {
    match error {
        UrlError::Invalid(what, at) => Value::variant(
            bv::URL_INVALID,
            [Value::string(what), Value::Int(at as i64)],
        ),
        UrlError::InvalidPart(what) => Value::variant(bv::URL_INVALID_PART, [Value::string(what)]),
    }
}

/// `Ok(value)` or `Err(UrlError)`.
fn result(made: Result<Value, UrlError>) -> Value {
    match made {
        Ok(value) => ok(value),
        Err(error) => Value::variant(bv::ERR, [error_value(error)]),
    }
}

/// The Url value of `u`, or the error of `name` when its text is longer
/// than a string may be (a `with_` function given a long part).
fn url_value(name: &str, u: Url) -> Result<Value, VmError> {
    match u.as_str().len() > MAX_TEXT_LEN {
        true => Err(too_long_text(name, u.as_str().len())),
        false => Ok(Value::Url(Arc::new(u))),
    }
}

/// [`url_value`] of a change that can be refused.
fn changed(name: &str, made: Result<Url, UrlError>) -> Result<Value, VmError> {
    Ok(result(match made {
        Ok(u) => Ok(url_value(name, u)?),
        Err(error) => Err(error),
    }))
}

/// A text the body made, or the error of `name` when it is longer than a
/// string may be.
fn text(name: &str, out: String) -> Result<String, VmError> {
    match out.len() > MAX_TEXT_LEN {
        true => Err(too_long_text(name, out.len())),
        false => Ok(out),
    }
}

/// The strings of a `List(String)`.
fn strings(name: &str, list: List) -> Result<Vec<String>, VmError> {
    list.iter()
        .map(|item| match item {
            Value::String(s, _) => Ok(s.to_string()),
            _ => Err(unsound(name, "segments")),
        })
        .collect()
}

/// The pairs of a `List((String, String))`.
fn pairs(name: &str, list: List) -> Result<Vec<(String, String)>, VmError> {
    list.iter()
        .map(|pair| match &pair {
            Value::Tuple(pair) => match &pair[..] {
                [Value::String(key, _), Value::String(value, _)] => {
                    Ok((key.to_string(), value.to_string()))
                }
                _ => Err(unsound(name, "pairs")),
            },
            _ => Err(unsound(name, "pairs")),
        })
        .collect()
}

fn string_pairs(pairs: Vec<(String, String)>) -> Value {
    Value::list(
        pairs
            .into_iter()
            .map(|(key, value)| Value::tuple(vec![Value::string(key), Value::string(value)]))
            .collect(),
    )
}

builtins! {
    // ── Making a Url ────────────────────────────────────────────────

    fn parse(text: &str) -> Value {
        result(url::parse(text).map(|u| Value::Url(Arc::new(u))))
    }

    fn resolve(base: typed::Url, reference: &str) -> Result<Value, VmError> {
        changed("url.resolve", url::resolve(base, reference))
    }

    // The whole text, user and password included.
    fn to_string(u: typed::Url) -> String {
        u.as_str().to_string()
    }

    // ── Reading a Url ───────────────────────────────────────────────

    fn scheme(u: typed::Url) -> String {
        u.scheme().to_string()
    }

    fn user(u: typed::Url) -> String {
        u.user()
    }

    fn password(u: typed::Url) -> String {
        u.password()
    }

    fn host(u: typed::Url) -> String {
        u.host().to_string()
    }

    fn port(u: typed::Url) -> Option<Value> {
        u.port().map(|port| Value::Int(i64::from(port)))
    }

    fn path(u: typed::Url) -> String {
        u.path().to_string()
    }

    fn segments(u: typed::Url) -> Value {
        Value::list(u.segments().into_iter().map(Value::string).collect())
    }

    fn query(u: typed::Url) -> Value {
        string_pairs(u.query())
    }

    fn query_text(u: typed::Url) -> String {
        u.query_text().to_string()
    }

    fn fragment(u: typed::Url) -> String {
        u.fragment()
    }

    // ── Changing a Url ──────────────────────────────────────────────

    fn with_scheme(u: typed::Url, scheme: &str) -> Result<Value, VmError> {
        changed("url.with_scheme", u.with_scheme(scheme))
    }

    fn with_user(u: typed::Url, user: &str) -> Result<Value, VmError> {
        url_value("url.with_user", u.with_user(user))
    }

    fn with_password(u: typed::Url, password: &str) -> Result<Value, VmError> {
        url_value("url.with_password", u.with_password(password))
    }

    fn with_host(u: typed::Url, host: &str) -> Result<Value, VmError> {
        changed("url.with_host", u.with_host(host))
    }

    fn with_port(u: typed::Url, port: &Value) -> Result<Value, VmError> {
        let port = match port {
            Value::Variant(some) if some.is(bv::SOME) => match some.fields() {
                [Value::Int(port)] => Some(*port),
                _ => return Err(unsound("url.with_port", "port")),
            },
            _ => None,
        };
        changed("url.with_port", u.with_port(port))
    }

    fn with_segments(u: typed::Url, segments: List) -> Result<Value, VmError> {
        let segments = strings("url.with_segments", segments)?;
        changed("url.with_segments", u.with_segments(&segments))
    }

    fn add_segments(u: typed::Url, segments: List) -> Result<Value, VmError> {
        let segments = strings("url.add_segments", segments)?;
        changed("url.add_segments", u.add_segments(&segments))
    }

    fn with_query(u: typed::Url, query: List) -> Result<Value, VmError> {
        let query = pairs("url.with_query", query)?;
        url_value("url.with_query", u.with_query(&query))
    }

    fn with_fragment(u: typed::Url, fragment: &str) -> Result<Value, VmError> {
        url_value("url.with_fragment", u.with_fragment(fragment))
    }

    // ── Escaping text ───────────────────────────────────────────────

    fn escape(s: &str) -> Result<String, VmError> {
        text("url.escape", url::escape(s))
    }

    fn unescape(s: &str) -> Value {
        result(url::unescape(s).map(Value::string))
    }

    // Pair by pair: the pairs can hold one long string many times, so
    // the text is stopped where it would pass the limit.
    fn encode_form(pairs: List) -> Result<String, VmError> {
        pairs.writable()?;
        let pairs = self::pairs("url.encode_form", pairs)?;
        let mut out = String::new();
        for (at, pair) in pairs.into_iter().enumerate() {
            if at > 0 {
                out.push('&');
            }
            out.push_str(&url::encode_form(&[pair]));
            if out.len() > MAX_TEXT_LEN {
                return Err(too_long_text("url.encode_form", out.len()));
            }
        }
        Ok(out)
    }

    fn decode_form(body: &str) -> Value {
        result(url::decode_form(body).map(string_pairs))
    }
}
