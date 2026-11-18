---
title: "url"
section: "Standard Library"
order: 27
---

# url

URLs as values of their own. A `url.Url` is made by `url.parse` from a text,
or from another `Url` by `url.resolve`, the `with_` functions and
`url.add_segments`. It is always a valid URL in one normal form: a URL can
be wrong only where it is parsed or changed, as a pattern can be wrong only
where `regex.compile` compiles it. Every function that reads a part of a
`Url` gives it bare, never an error.

The functions that change a URL escape what they are given, so a value put
into a URL stays one value: a name that holds `/`, `?`, `#`, `&` or `..` is
one path segment, not a path of its own.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let api = url.parse("https://API.example.com:443/v1/")?
  println(api) -- https://api.example.com/v1/
  println(url.segments(api)) -- ["v1", ""]

  let name = "ann/../admin?x=1"
  let user_page = api |> url.add_segments(["users", name])?
  let u = user_page |> url.with_query([("q", "silt lang"), ("tag", "a&b")])
  println(u) -- https://api.example.com/v1/users/ann%2F..%2Fadmin%3Fx=1?q=silt%20lang&tag=a%26b
  println(url.segments(u)) -- ["v1", "users", "ann/../admin?x=1"]
  println(url.path(u)) -- /v1/users/ann%2F..%2Fadmin%3Fx=1
  println(url.query(u)) -- [("q", "silt lang"), ("tag", "a&b")]
  println(url.query_text(u)) -- q=silt%20lang&tag=a%26b

  let next = url.resolve(u, "../docs?x=1#top")?
  println(next) -- https://api.example.com/v1/docs?x=1#top
  Ok(())
}
```

(A `?` that ends a pipeline applies to the whole pipeline; in the middle of
one it binds to the call before it, so a pipeline with a fallible step in the
middle is split into two `let`s, as above.)

## Which URLs

A `Url` is `scheme://[user[:password]@]host[:port]/path[?query][#fragment]`,
read by [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986). Every `Url` has
the `//` after its scheme, so its path always starts with `/`. A text without
it is refused: `mailto:ann@example.com`, `file:/x` (one slash), and
`localhost:8080`, which reads as the scheme `localhost`. Any scheme is taken;
`http.get` takes only `http` and `https`.

`url.parse` writes a URL in its normal form, and two texts that differ only
so are the same `Url` (`==`):

- the scheme and the host in lower case;
- the default port of the scheme dropped: 80 for `http` and `ws`, 443 for
  `https` and `wss`, 21 for `ftp` (no other scheme has one here: a
  `postgresql` URL keeps `:5432` if it is written);
- the hex digits of an escape in upper case (`%2f` is `%2F`), and an escape
  of a letter, a digit or `-` `.` `_` `~` decoded (`%41` is `A`);
- the `.` and `..` segments taken out of the path (`/a/./b/../c` is `/a/c`),
  and an empty path written `/`;
- an empty user and password (`http://@h/`), an empty password
  (`http://u:@h/` is `http://u@h/`), and an empty query or fragment (a `?` or
  a `#` alone) dropped. (RFC 3986 keeps `http://h/?` apart from `http://h/`;
  silt does not, because an empty query and no query read the same.)

Not the same `Url`: two queries in another order, `+` and `%20` in a query,
`%2F` and `/` in a path, a path with and without a `/` at its end, a host with
and without a `.` at its end.

A character that a URL cannot hold but that has one reading is escaped: a
space, every character that is not ASCII, and `"` `<` `>` `\` `^` `` ` ``
`{` `|` `}` `[` `]`, in the user, the password, the path, the query and the
fragment, and a `#` inside the fragment. So `https://h/a b/é?tag[]=a` is read
as `https://h/a%20b/%C3%A9?tag%5B%5D=a`. Refused are a control character
(a tab and a line feed too), a `%` that two hex digits do not follow, escapes
that are no UTF-8 text, white space at the start or the end of the text, and
more than one `@` before the path: a user or password holds `@` as `%40`.

A raw `/`, `?` or `#` in a password ends the part before the host there: no
reader can see that the text meant otherwise. Build such a URL with
`url.with_password`, which escapes them.

```silt
import list
import url

fn main() {
  let texts = [
    "localhost:8080",
    "http://0x7f.1/",
    "http://a@b@h/",
    "http://h/%zz",
    "http://h:99999/",
  ]
  list.each(texts) { text ->
    match url.parse(text) {
      Ok(u) -> println(u)
      Err(e) -> println(e)
    }
  }
  -- a URL has `//` after its scheme, at 10
  -- a host that looks like a number must be an IPv4 address (four numbers 0-255), at 7
  -- a user or password holds `@` as %40, at 10
  -- `%zz` is no escape, at 9
  -- port 99999 is not in 1..65535, at 9
}
```

### Hosts

The host names its machine plainly:

- a name is ASCII letters, digits, `-`, `_` and `.`, in lower case once
  read; it has no empty label (`a..b`, `.a`), no label over 63 characters and
  is at most 253 characters long. One `.` at its end is kept, and
  `example.com.` is another host than `example.com`, as in DNS;
- a host that looks like a number (its last label is all digits, or starts
  with `0x`) must be an IPv4 address of four numbers 0-255 with dots between
  and no leading zeros: `0x7f.1`, `2130706433`, `127.1` and `010.0.0.1`, which
  the system would read as some address, are refused, and so is `127.0.0.1.`;
- an IPv6 address is written in brackets, `[::1]`, and is kept in its short
  form (`[0:0:0:0:0:0:0:1]` is `[::1]`). One that holds an IPv4 address
  (`[::ffff:127.0.0.1]`, however it is written) is refused: write the IPv4
  address. A zone (`[fe80::1%25eth0]`) is refused;
- a host that is not ASCII is refused. For a name whose letters beyond ASCII
  are lower-case accented Latin letters (`é`, `ü`, `ñ`), the error names its
  ASCII form: `bücher.de` is `xn--bcher-kva.de`. For any other, write its
  ASCII (punycode) form, which starts with `xn--`;
- the host may be empty for a scheme other than `http`, `https`, `ws`, `wss`
  and `ftp`: `file:///etc/hosts`, and `postgresql://ann@/db` (user `ann` on
  the default socket).

What the rule does not do is say which addresses are this machine:
`0.0.0.0`, `[::]`, `127.0.0.2`, `localhost`, and a name that resolves to
`127.0.0.1` all reach it, each written plainly. A program that filters hosts
checks those itself. The text a `Url` shows is the address a connection
uses.

## Reading a `Url`

`url.user`, `url.password`, `url.fragment`, `url.segments` and `url.query`
give their part decoded; `""` (or `[]`) when the URL has none. `url.port` is
`None` when no port is written or it is the scheme's default; a program that
opens a socket itself takes the default port from the list above.

`url.segments` splits the path on `/` and decodes each segment: `/a%2Fb/c`
is `["a/b", "c"]`, `/a/` is `["a", ""]`, and `/` is `[]`. `url.query` reads
the query by the rules of forms (`+` is a space), in order, a key twice kept
twice, and `flag` as `("flag", "")`. `url.path` and `url.query_text` give
the path and the query as they stand in the URL, escaped: for a log line, for
a signature, and for a query that is not pairs (`?1234`), or whose reader
takes `+` as `+` (the options of a `postgresql` URL).

The path and the query keep their text as written, with only the escapes
normal: `?flag` stays `?flag` and `?a=1&a=2` keeps both. Only
`url.with_segments`, `url.add_segments` and `url.with_query` write them anew.

## Changing a `Url`

Each `with_` function gives a new `Url` with one part replaced, escaped:
the user and the password with every character but letters, digits and
`-` `.` `_` `~` escaped (so `:`, `@`, `/`, `?` and `#` in a password are
safe); a path segment with `/`, `%`, `?`, `#`, `[` and `]` escaped, so a
segment never becomes two; a query key or value with `&`, `=`, `+`, `#`,
`%`, `;`, `[` and `]` escaped and a space written `%20` (every reader takes
`%20` as a space; some take `+` as `+`). A part given as `""` (or `[]`)
removes it. Since a URL keeps its text as written, a part read from a text
and the same part given to a `with_` function can make two URLs that are
not equal: `url.parse("http://a!@h/")` keeps the `!` as it is and
`url.with_user` writes it `%21`, though `url.user` is `a!` for both.

`url.with_user`, `url.with_password`, `url.with_query` and
`url.with_fragment` cannot fail. `url.with_scheme`, `url.with_host` and
`url.with_port` check what they are given by the rules above;
`url.with_segments` and `url.add_segments` refuse a segment `.` or `..`, which
would be read back as a dot segment and taken out (and from a user's input is
a way up the path). `url.with_segments([])` and `url.with_segments([""])` are
the same URL, `/`.

`url.add_segments` puts segments after the URL's path, dropping a `/` at its
end first: `https://h/v1` and `https://h/v1/` with `["users", "ann"]` both give
`https://h/v1/users/ann`. A `""` last in the list keeps a `/` at the end, and
no segments give the URL as it is.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let db = url.parse("postgresql://localhost/app")?
    |> url.with_user("app")
    |> url.with_password("p@ss/w:rd")
  println(db) -- postgresql://***@localhost/app
  println(url.to_string(db)) -- postgresql://app:p%40ss%2Fw%3Ard@localhost/app
  println(url.password(db)) -- p@ss/w:rd
  let other = db |> url.with_port(Some(5433))?
  println(url.port(other)) -- Some(5433)
  Ok(())
}
```

## Resolving a reference

`url.resolve(base, reference)` gives the URL that `reference` names on the
page `base`, by RFC 3986 section 5: a whole URL, `//host/x` on the base's
scheme, `/x` on its host, a path next to the base's (`x`, `../x`), `?q` on
its path, `#f`, or `""` (the base without its fragment). The reference
follows the character rules above. A reference with a scheme but no `//`
(`a:b`, and RFC 3986's own `http:g`) is read as a URL and refused; write
`./a:b` for such a relative path. There is no value for a relative URL: a
reference is a `String` until it is resolved.

## Printing and storing

A `Url` is shown with its user and password hidden as `***@`:
`println(u)` and `"{u}"` write `https://***@example.com/x` for
`https://ann:s3cret@example.com/x`. An `@` in the path is shown as it is
(`https://h/users/@me`): the URL knows where its user is. `io.inspect`, a
`Url` inside a list, a record or a `Result`, and the message of a failed
`test.assert_eq` write it as `<url:https://***@example.com/x>`, so that it
does not read as a `String`. When the two values of a failed assertion
differ only in the hidden user or password, the message says so.

`url.to_string(u)` gives the whole text, user and password included: it is
the one way to hand a URL with its credentials to something that takes text
(`postgres.connect`, a header, a file). So, unlike `int.to_string`, it
differs from `"{u}"` when the URL has a user or a password. A URL put into a
text with `"{u}"` and then used carries `***`, and fails where it is used.

`==`, hashing and order are of the whole normal text, password included.
`json.stringify` writes a `Url` as its whole text, password included: json
is storage, and `json.parse` reads a field `site: url.Url` back with
`url.parse`, as `toml.parse`, `csv.parse` and a `postgres` text column do. A
map keyed by `Url` is written by `json.stringify` with the whole text as the
key. (`json.parse` reads a map's keys as `String` only: read such a map as
`Map(String, T)` and parse the keys.) A program that logs json of a URL hides
it itself.

A `Url` holds at most as many bytes as a string may hold (10,000,000; see
[how large a collection can be](../language/collections.md#how-large-a-collection-can-be)), counted in its normal text, which
escapes what it holds (`é` is six bytes of it): `url.parse` and the `with_`
functions stop the program at a longer one, as `string.repeat` does, and a
decoder refuses such a field as it refuses a text that is no URL.

```silt
import io
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://ann:s3cret@example.com/x")?
  println(u) -- https://***@example.com/x
  println("site: {u}") -- site: https://***@example.com/x
  println(io.inspect(u)) -- <url:https://***@example.com/x>
  println([u]) -- [<url:https://***@example.com/x>]
  println(url.to_string(u)) -- https://ann:s3cret@example.com/x
  println(u == url.parse("HTTPS://ann:s3cret@EXAMPLE.com:443/x")?) -- true
  Ok(())
}
```

## Escaping text

`url.escape`, `url.unescape`, `url.encode_form` and `url.decode_form` work
on one piece of text that is not a whole URL: a form body, the text a
signature is computed over (OAuth, AWS), a `filename*=` header parameter.
`url.escape` escapes every byte but letters, digits and `-` `.` `_` `~`;
`url.unescape` decodes `%HH` and leaves `+` as `+`. `url.encode_form` and
`url.decode_form` follow `application/x-www-form-urlencoded`: a space is `+`,
and a `+` is `%2B`. There is no function that escapes a whole URL: escaping
`https://h/a b` gives `https%3A%2F%2Fh%2Fa%20b`. Parse it instead.

```silt
import url

fn main() {
  println(url.escape("a b&c/é")) -- a%20b%26c%2F%C3%A9
  println(url.unescape("a%20b%26c")) -- Ok("a b&c")
  println(url.unescape("100%")) -- Err(`%` at the end is no escape, at 3)
  println(url.encode_form([("q", "silt lang"), ("page", "2")])) -- q=silt+lang&page=2
  println(url.decode_form("q=silt+lang&page=2&flag")) -- Ok([("q", "silt lang"), ("page", "2"), ("flag", "")])
}
```

## Errors

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Invalid(String, Int)` | what is wrong, and the character where: counted from 0, as `string.slice` counts; the text's length when it ends too early | the text is no URL, or no escaped text |
| `InvalidPart(String)` | what is wrong | a `with_` function was given a part the URL cannot hold |

An error does not repeat the text it was given, which may hold a password:
it names the character that is wrong, and at most a port that is out of
range or the ASCII form of a host. (That form is not named when an `@`
follows the host: what reads as the host may then be a token that a raw `/`
cut short.)

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(String) -> Result(Url, UrlError)` | Read a URL, in its normal form |
| `resolve` | `(Url, String) -> Result(Url, UrlError)` | The URL a reference (`../x`, `?q`, `//h/x`) names on the page `base` |
| `to_string` | `(Url) -> String` | The whole text, user and password included |
| `scheme` | `(Url) -> String` | The scheme: `https` |
| `user` | `(Url) -> String` | The user, decoded; `""` for none |
| `password` | `(Url) -> String` | The password, decoded; `""` for none |
| `host` | `(Url) -> String` | The host: a name, an IPv4 address, `[::1]`, or `""` |
| `port` | `(Url) -> Option(Int)` | The port; `None` when not written or the scheme's own |
| `path` | `(Url) -> String` | The path as it stands in the URL, escaped |
| `segments` | `(Url) -> List(String)` | The path's segments, decoded |
| `query` | `(Url) -> List((String, String))` | The query's pairs, decoded (`+` is a space) |
| `query_text` | `(Url) -> String` | The query as it stands in the URL, escaped, without `?` |
| `fragment` | `(Url) -> String` | The fragment, decoded; `""` for none |
| `with_scheme` | `(Url, String) -> Result(Url, UrlError)` | The URL with another scheme |
| `with_user` | `(Url, String) -> Url` | The URL with another user, escaped; `""` removes it |
| `with_password` | `(Url, String) -> Url` | The URL with another password, escaped; `""` removes it |
| `with_host` | `(Url, String) -> Result(Url, UrlError)` | The URL with another host |
| `with_port` | `(Url, Option(Int)) -> Result(Url, UrlError)` | The URL with another port; `None` for the scheme's own |
| `with_segments` | `(Url, List(String)) -> Result(Url, UrlError)` | The URL with the path of these segments, escaped |
| `add_segments` | `(Url, List(String)) -> Result(Url, UrlError)` | The URL with these segments, escaped, after its path |
| `with_query` | `(Url, List((String, String))) -> Url` | The URL with the query of these pairs, escaped; `[]` removes it |
| `with_fragment` | `(Url, String) -> Url` | The URL with another fragment, escaped; `""` removes it |
| `escape` | `(String) -> String` | Escape every byte but `A-Z a-z 0-9 - . _ ~` as `%HH` |
| `unescape` | `(String) -> Result(String, UrlError)` | Decode the `%HH` escapes of a text |
| `encode_form` | `(List((String, String))) -> String` | Build an `application/x-www-form-urlencoded` body |
| `decode_form` | `(String) -> Result(List((String, String)), UrlError)` | Read an `application/x-www-form-urlencoded` body into pairs |


## `url.parse`

```
url.parse(text: String) -> Result(Url, UrlError)
```

Reads a URL and writes it in its normal form (see "Which URLs" above).
`Err(url.Invalid(what, at))` for a text that is no URL.

```silt
import url

fn main() {
  println(url.parse("HTTP://Example.com:80/a/./b/../c")) -- Ok(<url:http://example.com/a/c>)
  println(url.parse("https://h/a b/é?tag[]=a")) -- Ok(<url:https://h/a%20b/%C3%A9?tag%5B%5D=a>)
  match url.parse("https://b\u{fc}cher.de/") {
    Ok(u) -> println(u)
    Err(e) -> println(e) -- a host name must be ASCII: write it as xn--bcher-kva.de, at 9
  }
}
```

## `url.resolve`

```
url.resolve(base: Url, reference: String) -> Result(Url, UrlError)
```

The URL that `reference` names on the page `base` (see "Resolving a
reference" above): what a link or a redirect's `Location` leads to.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let page = url.parse("https://h/docs/guide/intro?x=1#top")?
  println(url.resolve(page, "setup")?) -- https://h/docs/guide/setup
  println(url.resolve(page, "../api/")?) -- https://h/docs/api/
  println(url.resolve(page, "/home")?) -- https://h/home
  println(url.resolve(page, "?x=2")?) -- https://h/docs/guide/intro?x=2
  println(url.resolve(page, "//cdn.example.com/a.js")?) -- https://cdn.example.com/a.js
  println(url.resolve(page, "")?) -- https://h/docs/guide/intro?x=1
  Ok(())
}
```

## `url.to_string`

```
url.to_string(u: Url) -> String
```

The whole text of the URL, user and password included: what `url.parse`
reads back as the same `Url`. `"{u}"` hides the user and the password.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://ann:s3cret@example.com/x")?
  println("{u}") -- https://***@example.com/x
  println(url.to_string(u)) -- https://ann:s3cret@example.com/x
  Ok(())
}
```

## `url.scheme`, `url.host`, `url.port`

```
url.scheme(u: Url) -> String
url.host(u: Url) -> String
url.port(u: Url) -> Option(Int)
```

The scheme, in lower case; the host (a name in lower case, an IPv4 address,
an IPv6 address in brackets, or `""`); the port, `None` when none is written
or it is the scheme's default.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("HTTPS://Example.COM:8443/")?
  println(url.scheme(u)) -- https
  println(url.host(u)) -- example.com
  println(url.port(u)) -- Some(8443)
  println(url.port(url.parse("https://h:443/")?)) -- None
  println(url.host(url.parse("http://[0:0:0:0:0:0:0:1]/")?)) -- [::1]
  Ok(())
}
```

## `url.user`, `url.password`

```
url.user(u: Url) -> String
url.password(u: Url) -> String
```

The user and the password, decoded; `""` when there is none. The http client
sends them as an `authorization: Basic` header.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://ann:p%40ss@example.com/")?
  println(url.user(u)) -- ann
  println(url.password(u)) -- p@ss
  println(url.user(url.parse("https://example.com/")?) == "") -- true
  Ok(())
}
```

## `url.path`, `url.segments`

```
url.path(u: Url) -> String
url.segments(u: Url) -> List(String)
```

`url.path` is the path as it stands in the URL, escaped. `url.segments` is
the path split on `/`, each segment decoded: exact, so a `/` at the end is
a last `""`, and `%2F` is a `/` inside one segment; the path `/` alone is `[]`.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://h/users/a%20b/files%2Fold/")?
  println(url.path(u)) -- /users/a%20b/files%2Fold/
  println(url.segments(u)) -- ["users", "a b", "files/old", ""]
  println(url.segments(url.parse("https://h/")?)) -- []
  Ok(())
}
```

## `url.query`, `url.query_text`

```
url.query(u: Url) -> List((String, String))
url.query_text(u: Url) -> String
```

`url.query` gives the pairs of the query, decoded by the rules of forms (`+`
is a space), in order, a key twice kept twice; a key without `=` has the value
`""`. `url.query_text` is the query as it stands, escaped, without its `?`.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://h/search?q=silt+lang&tag=a&tag=b%26c&flag")?
  println(url.query(u)) -- [("q", "silt lang"), ("tag", "a"), ("tag", "b&c"), ("flag", "")]
  println(url.query_text(u)) -- q=silt+lang&tag=a&tag=b%26c&flag
  println(url.query(url.parse("https://h/")?)) -- []
  Ok(())
}
```

## `url.fragment`

```
url.fragment(u: Url) -> String
```

The fragment (after `#`), decoded; `""` when there is none.

```silt
import url

fn main() -> Result((), url.UrlError) {
  println(url.fragment(url.parse("https://h/doc#part%202")?)) -- part 2
  Ok(())
}
```

## `url.with_scheme`, `url.with_host`, `url.with_port`

```
url.with_scheme(u: Url, scheme: String) -> Result(Url, UrlError)
url.with_host(u: Url, host: String) -> Result(Url, UrlError)
url.with_port(u: Url, port: Option(Int)) -> Result(Url, UrlError)
```

The URL with another scheme, host or port, checked as `url.parse` checks
them: `Err(url.InvalidPart(why))` for one the URL cannot hold. A host is
written as `url.host` gives one, an IPv6 address in brackets. The port
`None`, or the scheme's default, writes none.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("http://localhost:8080/api")?
  println(url.with_scheme(u, "https")?) -- https://localhost:8080/api
  println(url.with_host(u, "Example.com")?) -- http://example.com:8080/api
  println(url.with_port(u, None)?) -- http://localhost/api
  println(url.with_port(u, Some(80))?) -- http://localhost/api
  println(url.with_port(u, Some(0))) -- Err(port 0 is not in 1..65535)
  println(url.with_host(u, "a b")) -- Err(a host cannot hold ' ')
  Ok(())
}
```

## `url.with_user`, `url.with_password`

```
url.with_user(u: Url, user: String) -> Url
url.with_password(u: Url, password: String) -> Url
```

The URL with another user or password, escaped: every character but letters,
digits and `-` `.` `_` `~` is written `%HH`, so any text is safe. `""`
removes it.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://example.com/")?
    |> url.with_user("ann")
    |> url.with_password("p@ss/w:rd")
  println(url.to_string(u)) -- https://ann:p%40ss%2Fw%3Ard@example.com/
  println(url.to_string(u |> url.with_password(""))) -- https://ann@example.com/
  Ok(())
}
```

## `url.with_segments`, `url.add_segments`

```
url.with_segments(u: Url, segments: List(String)) -> Result(Url, UrlError)
url.add_segments(u: Url, segments: List(String)) -> Result(Url, UrlError)
```

`url.with_segments` sets the whole path; `url.add_segments` puts the segments
after the path, a `/` at its end dropped first. Each segment is escaped, `/`
included, so it stays one segment. A segment `.` or `..` is refused:
`Err(url.InvalidPart(why))`.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let api = url.parse("https://h/v1/")?
  println(url.add_segments(api, ["users", "ann/x"])?) -- https://h/v1/users/ann%2Fx
  println(url.with_segments(api, ["status"])?) -- https://h/status
  println(url.with_segments(api, [])?) -- https://h/
  println(url.add_segments(api, ["users", ".."])) -- Err(a segment cannot be `..`)
  Ok(())
}
```

## `url.with_query`

```
url.with_query(u: Url, pairs: List((String, String))) -> Url
```

The URL with the query of `pairs`, each key and value escaped; a space is
written `%20`. `[]` removes the query.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://h/search?old=1")?
  println(url.with_query(u, [("q", "silt lang"), ("tag", "a&b=c")])) -- https://h/search?q=silt%20lang&tag=a%26b%3Dc
  println(url.with_query(u, [])) -- https://h/search
  Ok(())
}
```

## `url.with_fragment`

```
url.with_fragment(u: Url, fragment: String) -> Url
```

The URL with another fragment, escaped; `""` removes it.

```silt
import url

fn main() -> Result((), url.UrlError) {
  let u = url.parse("https://h/doc")?
  println(url.with_fragment(u, "part 2")) -- https://h/doc#part%202
  Ok(())
}
```

## `url.escape`

```
url.escape(text: String) -> String
```

Escapes every byte of the text's UTF-8 but letters, digits and `-` `.` `_`
`~` as `%HH`, upper-case hex. `+` is escaped too (`%2B`). For one piece of
text that is not a whole URL; the `with_` functions escape their parts
themselves.

```silt
import url

fn main() {
  println(url.escape("a b&c/é")) -- a%20b%26c%2F%C3%A9
  println(url.escape("1+1")) -- 1%2B1
}
```

## `url.unescape`

```
url.unescape(text: String) -> Result(String, UrlError)
```

Decodes the `%HH` escapes of a text. A `+` stays a `+`. A `%` that two hex
digits do not follow, or escapes whose bytes are no UTF-8 text, are
`Err(url.Invalid(what, at))`.

```silt
import url

fn main() {
  println(url.unescape("a%20b%26c")) -- Ok("a b&c")
  match url.unescape("100%") {
    Ok(text) -> println(text)
    Err(url.Invalid(what, at)) -> println("{what}, at {at}") -- `%` at the end is no escape, at 3
    Err(e) -> println(e)
  }
}
```

## `url.encode_form`

```
url.encode_form(pairs: List((String, String))) -> String
```

An `application/x-www-form-urlencoded` body: `key=value` pairs joined by
`&`, a space written `+`, every byte but letters, digits and `*` `-` `.` `_`
written `%HH`.

```silt
import url

fn main() {
  println(url.encode_form([("q", "silt lang"), ("page", "2")])) -- q=silt+lang&page=2
}
```

## `url.decode_form`

```
url.decode_form(body: String) -> Result(List((String, String)), UrlError)
```

The pairs of an `application/x-www-form-urlencoded` body: split on `&` (an
empty piece is skipped), each on its first `=` (a piece without one has the
value `""`), `+` read as a space and escapes decoded.

```silt
import url

fn main() {
  println(url.decode_form("q=silt+lang&page=2&flag")) -- Ok([("q", "silt lang"), ("page", "2"), ("flag", "")])
  println(url.decode_form("a=%zz")) -- Err(`%zz` is no escape, at 2)
}
```
