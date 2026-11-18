use super::*;

fn url(text: &str) -> Url {
    match parse(text) {
        Ok(u) => u,
        Err(e) => panic!("{text:?} does not parse: {e}"),
    }
}

/// The text `text` reads as, which must read as itself again.
fn normal(text: &str) -> String {
    let u = url(text);
    let again = url(u.as_str());
    assert_eq!(again, u, "{text:?} is not read back as itself");
    u.as_str().to_string()
}

fn refusal(text: &str) -> (String, usize) {
    match parse(text) {
        Ok(u) => panic!("{text:?} is read as {u:?}"),
        Err(UrlError::Invalid(what, at)) => {
            assert!(
                at <= text.chars().count(),
                "{text:?}: place {at} is past its end"
            );
            (what, at)
        }
        Err(e) => panic!("{text:?}: {e:?}"),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

// ── Normal form ─────────────────────────────────────────────────────

#[test]
fn parse_writes_the_normal_form() {
    for (text, want) in [
        ("HTTP://H:80/a/./b", "http://h/a/b"),
        (
            "https://API.example.com:443/v1/",
            "https://api.example.com/v1/",
        ),
        ("http://h", "http://h/"),
        ("http://h?x=1", "http://h/?x=1"),
        ("http://h#f", "http://h/#f"),
        ("ws://h:80/", "ws://h/"),
        ("wss://h:443/", "wss://h/"),
        ("ftp://h:21/", "ftp://h/"),
        ("http://h:443/", "http://h:443/"),
        ("postgresql://h:5432/db", "postgresql://h:5432/db"),
        ("http://h:0080/", "http://h/"),
        ("http://h:08080/", "http://h:8080/"),
        ("http://h/%2f%7e%41%2E", "http://h/%2F~A."),
        ("http://h/a/%2E%2E/b", "http://h/b"),
        ("http://h/a/%2e/b", "http://h/a/b"),
        ("http://h/a/b/..", "http://h/a/"),
        ("http://h/a/b/.", "http://h/a/b/"),
        ("http://h/..", "http://h/"),
        ("http://h/../../x", "http://h/x"),
        ("http://h/a/..//x", "http://h//x"),
        ("http://h//x", "http://h//x"),
        ("http://@h/", "http://h/"),
        ("http://:@h/", "http://h/"),
        ("http://u:@h/", "http://u@h/"),
        ("http://:p@h/", "http://:p@h/"),
        ("http://u:p:q@h/", "http://u:p:q@h/"),
        ("http://h/?", "http://h/"),
        ("http://h/#", "http://h/"),
        ("http://h/?#", "http://h/"),
        ("http://h/a b/é", "http://h/a%20b/%C3%A9"),
        ("https://h/q?tag[]=a", "https://h/q?tag%5B%5D=a"),
        ("http://h/a\\b", "http://h/a%5Cb"),
        ("http://h/\"<>^`{|}", "http://h/%22%3C%3E%5E%60%7B%7C%7D"),
        ("http://h/#a#b", "http://h/#a%23b"),
        ("http://h/?a=1?b/c", "http://h/?a=1?b/c"),
        ("http://h/x?q=a b", "http://h/x?q=a%20b"),
        ("http://h/x?flag", "http://h/x?flag"),
        ("http://h/x?a=1&a=2", "http://h/x?a=1&a=2"),
        ("http://h/x?a+b", "http://h/x?a+b"),
        ("http://us er:pä@h/", "http://us%20er:p%C3%A4@h/"),
        ("http://h/!$&'()*+,;=:@", "http://h/!$&'()*+,;=:@"),
        ("http://example.com./", "http://example.com./"),
        ("http://a_b-c.d/", "http://a_b-c.d/"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/"),
        ("http://[0:0:0:0:0:0:0:1]/", "http://[::1]/"),
        (
            "http://[2001:DB8:0:0:1:0:0:1]:80/",
            "http://[2001:db8::1:0:0:1]/",
        ),
        ("http://[1:0:0:2:0:0:0:3]/", "http://[1:0:0:2::3]/"),
        ("http://[::]/", "http://[::]/"),
        ("http://[64:ff9b::127.0.0.1]/", "http://[64:ff9b::7f00:1]/"),
        ("file:///x", "file:///x"),
        ("file://", "file:///"),
        ("postgresql://ann@/db", "postgresql://ann@/db"),
        ("postgresql://:5433/db", "postgresql://:5433/db"),
        ("A+b-c.d://h/", "a+b-c.d://h/"),
    ] {
        assert_eq!(normal(text), want, "{text:?}");
    }
}

#[test]
fn urls_that_differ_only_by_normalising_are_equal() {
    assert_eq!(url("HTTP://H:80/a/./b"), url("http://h/a/b"));
    assert_eq!(url("http://h/%7e"), url("http://h/~"));
    assert_eq!(url("http://h/?"), url("http://h/"));
    // Not the same: query order, `+` against `%20`, `%2F` against `/`, a
    // trailing `/`, a host's trailing dot.
    for (a, b) in [
        ("http://h/?a=1&b=2", "http://h/?b=2&a=1"),
        ("http://h/?a=b+c", "http://h/?a=b%20c"),
        ("http://h/a%2Fb", "http://h/a/b"),
        ("http://h/a/", "http://h/a"),
        ("http://h.com./", "http://h.com/"),
    ] {
        assert_ne!(url(a), url(b), "{a} {b}");
    }
}

#[test]
fn order_and_hash_are_of_the_whole_text() {
    use std::collections::HashSet;
    assert!(url("http://a/") < url("http://b/"));
    assert!(url("http://u:a@h/") < url("http://u:b@h/"));
    let set: HashSet<Url> = [url("http://h:80/"), url("HTTP://h/")]
        .into_iter()
        .collect();
    assert_eq!(set.len(), 1);
}

// ── Refusals ────────────────────────────────────────────────────────

#[test]
fn the_refusals_say_what_and_where() {
    for (text, what, at) in [
        ("localhost:8080", "a URL has `//` after its scheme", 10),
        (
            "mailto:ann@example.com",
            "a URL has `//` after its scheme",
            7,
        ),
        ("file:/x", "a URL has `//` after its scheme", 5),
        ("http:example.com", "a URL has `//` after its scheme", 5),
        (
            "http://0x7f.1/",
            "a host that looks like a number must be an IPv4 address (four numbers 0-255)",
            7,
        ),
        (
            "http://[::ffff:127.0.0.1]/",
            "an IPv6 address that holds an IPv4 address: write the IPv4 address",
            7,
        ),
        ("http://a@b@h/", "a user or password holds `@` as %40", 10),
        (
            "https://b\u{fc}cher.de/",
            "a host name must be ASCII: write it as xn--bcher-kva.de",
            9,
        ),
        ("http://h/%zz", "`%zz` is no escape", 9),
        ("http://h/caf%E9", "the escape %E9 is no UTF-8 text", 12),
        (
            "https://h/caf%E9?name=%FF",
            "the escape %E9 is no UTF-8 text",
            13,
        ),
        ("http://h:99999/", "port 99999 is not in 1..65535", 9),
        (" http://h/", "a URL does not start with a space", 0),
        ("http://a..b/", "a host name has no empty label", 9),
    ] {
        assert_eq!(refusal(text), (what.to_string(), at), "{text:?}");
    }
}

#[test]
fn a_text_without_a_scheme_is_refused() {
    for text in [
        "",
        "/x",
        "//h/x",
        "example.com",
        "1http://h/",
        "?x",
        ":x",
        "a/b:c",
    ] {
        assert_eq!(refusal(text), (NO_SCHEME.to_string(), 0), "{text:?}");
    }
    assert_eq!(
        refusal("ht tp://h/"),
        ("a scheme cannot hold ' '".to_string(), 2)
    );
    assert_eq!(
        refusal("ht_tp://h/"),
        ("a scheme cannot hold '_'".to_string(), 2)
    );
}

#[test]
fn white_space_at_the_ends_and_controls_are_refused() {
    assert_eq!(
        refusal("http://h/ "),
        ("a URL does not end with a space".to_string(), 9)
    );
    assert_eq!(
        refusal("\u{a0}http://h/"),
        (
            "a URL does not start with the space character U+00A0".to_string(),
            0
        )
    );
    for (text, at) in [
        ("http://exa\tmple.com/", 10),
        ("http://h/a\tb", 10),
        ("http://h/\n", 9),
        ("\thttp://h/", 0),
        ("http://h/\u{7f}", 9),
        ("http://h/é\u{1}", 10),
    ] {
        let (what, place) = refusal(text);
        assert!(
            what.starts_with("a URL cannot hold the control character U+00"),
            "{what}"
        );
        assert_eq!(place, at, "{text:?}");
    }
}

#[test]
fn bad_escapes_are_refused_in_every_part() {
    for (text, what, at) in [
        ("http://h/a%", "`%` at the end is no escape", 10),
        ("http://h/a%4", "`%4` at the end is no escape", 10),
        ("http://h/a%4?x", "`%4?` is no escape", 10),
        (
            "http://h/a%\u{e9}x",
            "a `%` without two hex digits is no escape",
            10,
        ),
        ("http://u%zz@h/", "`%zz` is no escape", 8),
        ("http://u:p%zz@h/", "`%zz` is no escape", 10),
        ("http://h/?q=%zz", "`%zz` is no escape", 12),
        ("http://h/#%zz", "`%zz` is no escape", 10),
        ("http://u%FF@h/", "the escape %FF is no UTF-8 text", 8),
        ("http://h/?a=%c3x", "the escape %C3 is no UTF-8 text", 12),
        ("http://h/#%80", "the escape %80 is no UTF-8 text", 10),
        // A character's bytes split by a `/` are no text either side.
        ("http://h/%C3/%A9", "the escape %C3 is no UTF-8 text", 9),
    ] {
        assert_eq!(refusal(text), (what.to_string(), at), "{text:?}");
    }
    // Escapes of a whole character are text.
    assert_eq!(normal("http://h/%c3%a9"), "http://h/%C3%A9");
}

#[test]
fn hosts_name_their_machine_plainly() {
    let number_like = (NUMBER_LIKE.to_string(), 7);
    for text in [
        "http://0x7f.1/",
        "http://2130706433/",
        "http://127.1/",
        "http://010.0.0.1/",
        "http://256.0.0.1/",
        "http://1.2.3.4.5/",
        "http://0x7f.1./",
        "http://a.1/",
        "http://a.0xg/",
        "http://0X7F.0.0.1/",
    ] {
        assert_eq!(refusal(text), number_like, "{text:?}");
    }
    assert_eq!(
        refusal("http://127.0.0.1./"),
        ("an IPv4 address has no `.` at its end".to_string(), 7)
    );
    for text in [
        "http://[::ffff:127.0.0.1]/",
        "http://[::ffff:7f00:1]/",
        "http://[::127.0.0.1]/",
        "http://[::2]/",
    ] {
        assert_eq!(refusal(text), (HOLDS_IPV4.to_string(), 7), "{text:?}");
    }
    for text in [
        "http://0.0.0.0/",
        "http://[::]/",
        "http://[::1]/",
        "http://localhost/",
    ] {
        assert_eq!(normal(text), text);
    }
}

#[test]
fn host_faults() {
    for (text, what, at) in [
        ("http:///x", "a URL of scheme `http` has a host", 7),
        ("https://u@/x", "a URL of scheme `https` has a host", 10),
        ("http://h:/", "a `:` after the host has no port", 9),
        ("http://h:0/", "port 0 is not in 1..65535", 9),
        ("http://h:000/", "port 0 is not in 1..65535", 9),
        ("http://h:65536/", "port 65536 is not in 1..65535", 9),
        ("http://h:8a/", "a port cannot hold 'a'", 10),
        ("http://h:80:90/", "a port cannot hold ':'", 11),
        (
            "http://h:123456789012345/",
            "a port is a number in 1..65535",
            9,
        ),
        ("http://exa mple.com/", "a host cannot hold ' '", 10),
        ("http://exa%41mple/", "a host cannot hold '%'", 10),
        ("http://h\\evil/", "a host cannot hold '\\'", 8),
        ("http://a[b]/", "a host cannot hold '['", 8),
        ("http://.a/", "a host name has no empty label", 7),
        ("http://a../", "a host name has no empty label", 9),
        ("http://./", "a host name has no empty label", 7),
        ("http://[::1/", "the `[` of an IPv6 address has no `]`", 7),
        (
            "http://[::1]x/",
            "after the `]` of an IPv6 address comes `:` and a port, or the path",
            12,
        ),
        (
            "http://[fe80::1%25eth0]/",
            "an IPv6 address with a zone (`%`) is not taken",
            15,
        ),
        ("http://[v1.x]/", "the brackets hold no IPv6 address", 7),
        ("http://[1.2.3.4]/", "the brackets hold no IPv6 address", 7),
        ("http://[]/", "the brackets hold no IPv6 address", 7),
    ] {
        assert_eq!(refusal(text), (what.to_string(), at), "{text:?}");
    }
    let label = "a".repeat(64);
    assert_eq!(
        refusal(&format!("http://x.{label}/")),
        (
            "a label of a host name is at most 63 characters long".to_string(),
            9
        )
    );
    normal(&format!("http://x.{}/", "a".repeat(63)));
    let long = vec!["a".repeat(63); 4].join(".");
    assert_eq!(long.len(), 255);
    assert_eq!(
        refusal(&format!("http://{long}/")),
        ("a host name is at most 253 characters long".to_string(), 7)
    );
    normal(&format!("http://{}./", &long[..253]));
}

#[test]
fn the_punycode_form_is_named_only_where_it_is_sure() {
    let hint = |host: &str| refusal(&format!("http://{host}/")).0;
    assert_eq!(
        hint("b\u{fc}cher.de"),
        "a host name must be ASCII: write it as xn--bcher-kva.de"
    );
    assert_eq!(
        hint("M\u{fc}nchen.caf\u{e9}.de"),
        "a host name must be ASCII: write it as xn--mnchen-3ya.xn--caf-dma.de"
    );
    assert_eq!(
        hint("\u{17c}\u{f3}\u{142}\u{107}.pl"),
        "a host name must be ASCII: write it as xn--kda4b0koi.pl"
    );
    let unsure = "a host name must be ASCII: write its punycode form (xn--...)";
    // Upper case, ß, the long s and other scripts are mapped by IDNA's
    // tables, which silt does not have.
    for host in [
        "B\u{dc}CHER.de",
        "stra\u{df}e.de",
        "\u{17f}.de",
        "\u{4f8b}.jp",
        "\u{ff}.de",
    ] {
        assert_eq!(hint(host), unsure, "{host}");
    }
    // A token before a raw `/` would read as the host: an `@` after the
    // host's start names no ASCII form.
    assert_eq!(refusal("https://t\u{f6}k\u{e9}n/x@h/").0, unsure);
    assert_eq!(
        refusal("https://u:p@b\u{fc}cher.de/").0,
        "a host name must be ASCII: write it as xn--bcher-kva.de"
    );
    assert_eq!(punycode("\u{fc}").as_deref(), Some("tda"));
    assert_eq!(punycode("espa\u{f1}a").as_deref(), Some("espaa-rta"));
    assert_eq!(punycode("\u{138}a").as_deref(), Some("a-ska"));
    assert_eq!(
        punycode("a\u{105}\u{10d}\u{119}\u{117}\u{12f}\u{161}\u{173}\u{16b}\u{17e}").as_deref(),
        Some("a-whay4bj7koouc7cvf")
    );
}

// ── Readers ─────────────────────────────────────────────────────────

#[test]
fn the_readers_give_the_parts() {
    let u = url(
        "https://ann:s%3Acret@Example.com:8443/a%20b/c%2Fd/?q=silt+lang&tag=a%26b&flag#top%20x",
    );
    assert_eq!(u.scheme(), "https");
    assert_eq!(u.user(), "ann");
    assert_eq!(u.password(), "s:cret");
    assert_eq!(u.host(), "example.com");
    assert_eq!(u.port(), Some(8443));
    assert_eq!(u.path(), "/a%20b/c%2Fd/");
    assert_eq!(u.segments(), strings(&["a b", "c/d", ""]));
    assert_eq!(u.query_text(), "q=silt+lang&tag=a%26b&flag");
    assert_eq!(
        u.query(),
        pairs(&[("q", "silt lang"), ("tag", "a&b"), ("flag", "")])
    );
    assert_eq!(u.fragment(), "top x");
    let bare = url("http://h:80");
    assert_eq!(
        (bare.user(), bare.password(), bare.port()),
        (String::new(), String::new(), None)
    );
    assert_eq!((bare.query_text(), bare.fragment()), ("", String::new()));
    assert!(bare.query().is_empty());
}

#[test]
fn segments_are_exact_but_at_the_root() {
    for (text, want) in [
        ("http://h/", vec![]),
        ("http://h/a/b", vec!["a", "b"]),
        ("http://h/a/", vec!["a", ""]),
        ("http://h/a%2Fb/c", vec!["a/b", "c"]),
        ("http://h//x", vec!["", "x"]),
    ] {
        assert_eq!(url(text).segments(), strings(&want), "{text}");
    }
    assert_eq!(
        url("http://h/?a=1&&b&=c&d=e=f").query(),
        pairs(&[("a", "1"), ("b", ""), ("", "c"), ("d", "e=f")])
    );
}

#[test]
fn display_hides_the_user_and_password_exactly() {
    for (text, shown) in [
        (
            "https://ann:s3cret@example.com/x",
            "https://***@example.com/x",
        ),
        ("https://TOKEN@github.com/", "https://***@github.com/"),
        ("http://:p@h/", "http://***@h/"),
        ("https://h/users/@me", "https://h/users/@me"),
        ("postgresql://ann@/db", "postgresql://***@/db"),
    ] {
        assert_eq!(url(text).to_string(), shown, "{text}");
    }
    assert_eq!(
        url("https://ann:s3cret@example.com/x").as_str(),
        "https://ann:s3cret@example.com/x"
    );
}

// ── Changing a Url ──────────────────────────────────────────────────

#[test]
fn the_design_example_holds() {
    let api = url("https://API.example.com:443/v1/");
    assert_eq!(api.to_string(), "https://api.example.com/v1/");
    assert_eq!(api.segments(), strings(&["v1", ""]));
    let user_page = api
        .add_segments(&strings(&["users", "ann/../admin?x=1"]))
        .unwrap();
    let u = user_page.with_query(&pairs(&[("q", "silt lang"), ("tag", "a&b")]));
    assert_eq!(
        u.as_str(),
        "https://api.example.com/v1/users/ann%2F..%2Fadmin%3Fx=1?q=silt%20lang&tag=a%26b"
    );
    assert_eq!(u.segments(), strings(&["v1", "users", "ann/../admin?x=1"]));
    assert_eq!(u.path(), "/v1/users/ann%2F..%2Fadmin%3Fx=1");
    assert_eq!(u.query(), pairs(&[("q", "silt lang"), ("tag", "a&b")]));
    assert_eq!(u.query_text(), "q=silt%20lang&tag=a%26b");
    let db = url("postgresql://localhost/app")
        .with_user("app")
        .with_password("p@ss/w:rd");
    assert_eq!(db.to_string(), "postgresql://***@localhost/app");
    assert_eq!(
        db.as_str(),
        "postgresql://app:p%40ss%2Fw%3Ard@localhost/app"
    );
    let next = resolve(&u, "../docs?x=1#top").unwrap();
    assert_eq!(next.as_str(), "https://api.example.com/v1/docs?x=1#top");
}

#[test]
fn add_segments_puts_them_under_the_base() {
    for (base, want) in [
        ("https://h/v1", "https://h/v1/users/ann"),
        ("https://h/v1/", "https://h/v1/users/ann"),
        ("https://h/", "https://h/users/ann"),
        ("https://h/a//", "https://h/a//users/ann"),
        ("https://h/v1/?q=1#f", "https://h/v1/users/ann?q=1#f"),
    ] {
        let u = url(base).add_segments(&strings(&["users", "ann"])).unwrap();
        assert_eq!(u.as_str(), want, "{base}");
    }
    let trailing = url("https://h/v1").add_segments(&strings(&["users", ""]));
    assert_eq!(trailing.unwrap().as_str(), "https://h/v1/users/");
    assert_eq!(
        url("https://h/v1/").add_segments(&[]).unwrap().as_str(),
        "https://h/v1/"
    );
    for dots in [".", ".."] {
        assert_eq!(
            url("https://h/v1").add_segments(&strings(&["a", dots])),
            Err(UrlError::InvalidPart(format!(
                "a segment cannot be `{dots}`"
            )))
        );
    }
}

#[test]
fn with_segments_sets_the_whole_path() {
    let u = url("http://h/old/path?q=1");
    for (segments, path) in [
        (vec![], "/"),
        (vec![""], "/"),
        (vec!["a", ""], "/a/"),
        (vec!["", "x"], "//x"),
        (
            vec!["a b", "c/d", "%", "é", "[x]", "!$&'()*+,;=:@"],
            "/a%20b/c%2Fd/%25/%C3%A9/%5Bx%5D/!$&'()*+,;=:@",
        ),
        (vec![".a", "..."], "/.a/..."),
    ] {
        let changed = u.with_segments(&strings(&segments)).unwrap();
        assert_eq!(changed.path(), path, "{segments:?}");
        assert_eq!(changed.query_text(), "q=1");
        assert_eq!(url(changed.as_str()), changed);
        if segments != [""] {
            assert_eq!(changed.segments(), strings(&segments));
        }
    }
    assert!(u.with_segments(&strings(&[".."])).is_err());
}

#[test]
fn the_parts_are_escaped_when_written() {
    let u = url("http://h/");
    let all = " !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~é";
    let changed = u
        .with_user(all)
        .with_password(all)
        .with_query(&pairs(&[(all, all), ("", "")]))
        .with_fragment(all);
    let user = "%20%21%22%23%24%25%26%27%28%29%2A%2B%2C-.%2F%3A%3B%3C%3D%3E%3F%40%5B%5C%5D%5E_%60%7B%7C%7D~%C3%A9";
    let query = "%20!%22%23$%25%26'()*%2B,-./:%3B%3C%3D%3E?@%5B%5C%5D%5E_%60%7B%7C%7D~%C3%A9";
    let fragment = "%20!%22%23$%25&'()*+,-./:;%3C=%3E?@%5B%5C%5D%5E_%60%7B%7C%7D~%C3%A9";
    assert_eq!(
        changed.as_str(),
        format!("http://{user}:{user}@h/?{query}={query}&=#{fragment}")
    );
    assert_eq!(url(changed.as_str()), changed);
    assert_eq!(changed.user(), all);
    assert_eq!(changed.password(), all);
    assert_eq!(changed.query(), pairs(&[(all, all), ("", "")]));
    assert_eq!(changed.fragment(), all);
    // A space in a query is written %20, and read back as a space.
    let q = u.with_query(&pairs(&[("q", "a b+c")]));
    assert_eq!(q.query_text(), "q=a%20b%2Bc");
    assert_eq!(q.query(), pairs(&[("q", "a b+c")]));
}

#[test]
fn an_empty_part_removes_it() {
    let u = url("http://u:p@h/x?q=1#f");
    assert_eq!(u.with_user("").as_str(), "http://:p@h/x?q=1#f");
    assert_eq!(u.with_password("").as_str(), "http://u@h/x?q=1#f");
    assert_eq!(
        u.with_user("").with_password("").as_str(),
        "http://h/x?q=1#f"
    );
    assert_eq!(u.with_query(&[]).as_str(), "http://u:p@h/x#f");
    assert_eq!(u.with_fragment("").as_str(), "http://u:p@h/x?q=1");
    // An empty host keeps its user, password and port.
    let file = url("file:///x");
    assert_eq!(file.with_user("ann").as_str(), "file://ann@/x");
    assert_eq!(
        file.with_password("pw")
            .with_port(Some(9))
            .unwrap()
            .as_str(),
        "file://:pw@:9/x"
    );
    assert_eq!(url("file://:pw@:9/x").as_str(), "file://:pw@:9/x");
}

#[test]
fn with_scheme_host_and_port_check_what_they_are_given() {
    let u = url("http://h:8080/x");
    assert_eq!(u.with_scheme("HTTPS").unwrap().as_str(), "https://h:8080/x");
    assert_eq!(
        url("https://h:80/").with_scheme("http").unwrap().as_str(),
        "http://h/"
    );
    for (scheme, why) in [
        ("", "a scheme starts with a letter"),
        ("1x", "a scheme starts with a letter"),
        ("ht tp", "a scheme cannot hold ' '"),
        ("htt\u{e9}", "a scheme cannot hold '\u{e9}'"),
    ] {
        assert_eq!(
            u.with_scheme(scheme),
            Err(UrlError::InvalidPart(why.into()))
        );
    }
    assert_eq!(
        url("file:///x").with_scheme("http"),
        Err(UrlError::InvalidPart(
            "a URL of scheme `http` has a host".into()
        ))
    );
    assert_eq!(
        u.with_host("Example.COM").unwrap().as_str(),
        "http://example.com:8080/x"
    );
    assert_eq!(
        u.with_host("[0::1]").unwrap().as_str(),
        "http://[::1]:8080/x"
    );
    for (host, why) in [
        ("h:1", "a host cannot hold ':'"),
        ("a b", "a host cannot hold ' '"),
        ("", "a URL of scheme `http` has a host"),
        ("0x7f.1", NUMBER_LIKE),
        ("[::ffff:1.2.3.4]", HOLDS_IPV4),
        ("::1", "a host cannot hold ':'"),
    ] {
        assert_eq!(
            u.with_host(host),
            Err(UrlError::InvalidPart(why.into())),
            "{host}"
        );
    }
    assert_eq!(
        url("file:///x").with_host("").unwrap().as_str(),
        "file:///x"
    );
    assert_eq!(u.with_port(None).unwrap().as_str(), "http://h/x");
    assert_eq!(u.with_port(Some(80)).unwrap().port(), None);
    assert_eq!(
        u.with_port(Some(65535)).unwrap().as_str(),
        "http://h:65535/x"
    );
    for port in [0, -1, 65536, i64::MAX] {
        assert_eq!(
            u.with_port(Some(port)),
            Err(UrlError::InvalidPart(format!(
                "port {port} is not in 1..65535"
            )))
        );
    }
}

// ── Resolving ───────────────────────────────────────────────────────

#[test]
fn rfc_3986_resolution_examples() {
    let base = url("http://a/b/c/d;p?q");
    for (reference, want) in [
        // 5.4.1, normal examples ("g:h" is refused: no `//`).
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g/"),
        ("?y", "http://a/b/c/d;p?y"),
        ("g?y", "http://a/b/c/g?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("g#s", "http://a/b/c/g#s"),
        ("g?y#s", "http://a/b/c/g?y#s"),
        (";x", "http://a/b/c/;x"),
        ("g;x", "http://a/b/c/g;x"),
        ("g;x?y#s", "http://a/b/c/g;x?y#s"),
        ("", "http://a/b/c/d;p?q"),
        (".", "http://a/b/c/"),
        ("./", "http://a/b/c/"),
        ("..", "http://a/b/"),
        ("../", "http://a/b/"),
        ("../g", "http://a/b/g"),
        ("../..", "http://a/"),
        ("../../", "http://a/"),
        ("../../g", "http://a/g"),
        // 5.4.2, abnormal examples ("http:g" is refused).
        ("../../../g", "http://a/g"),
        ("../../../../g", "http://a/g"),
        ("/./g", "http://a/g"),
        ("/../g", "http://a/g"),
        ("g.", "http://a/b/c/g."),
        (".g", "http://a/b/c/.g"),
        ("g..", "http://a/b/c/g.."),
        ("..g", "http://a/b/c/..g"),
        ("./../g", "http://a/b/g"),
        ("./g/.", "http://a/b/c/g/"),
        ("g/./h", "http://a/b/c/g/h"),
        ("g/../h", "http://a/b/c/h"),
        ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
        ("g;x=1/../y", "http://a/b/c/y"),
        ("g?y/./x", "http://a/b/c/g?y/./x"),
        ("g?y/../x", "http://a/b/c/g?y/../x"),
        ("g#s/./x", "http://a/b/c/g#s/./x"),
        ("g#s/../x", "http://a/b/c/g#s/../x"),
    ] {
        let resolved = resolve(&base, reference).unwrap();
        assert_eq!(resolved.as_str(), want, "{reference:?}");
        assert_eq!(url(resolved.as_str()), resolved, "{reference:?}");
    }
    for reference in ["g:h", "http:g"] {
        let at = reference.find(':').unwrap() + 1;
        assert_eq!(
            resolve(&base, reference),
            Err(UrlError::Invalid(
                "a URL has `//` after its scheme".into(),
                at
            ))
        );
    }
}

#[test]
fn resolve_keeps_the_base_and_checks_the_reference() {
    let base = url("https://u:p@h:8443/a/b?q#f");
    assert_eq!(
        resolve(&base, "").unwrap().as_str(),
        "https://u:p@h:8443/a/b?q"
    );
    assert_eq!(
        resolve(&base, "?").unwrap().as_str(),
        "https://u:p@h:8443/a/b"
    );
    assert_eq!(
        resolve(&base, "c d/é").unwrap().as_str(),
        "https://u:p@h:8443/a/c%20d/%C3%A9"
    );
    assert_eq!(
        resolve(&base, "./a:b").unwrap().as_str(),
        "https://u:p@h:8443/a/a:b"
    );
    assert_eq!(
        resolve(&base, "%2E%2E/x").unwrap().as_str(),
        "https://u:p@h:8443/x"
    );
    assert_eq!(
        resolve(&base, "HTTP://X:80/y").unwrap().as_str(),
        "http://x/y"
    );
    assert_eq!(
        resolve(&base, "//other/z").unwrap().as_str(),
        "https://other/z"
    );
    // (No scheme starts with `é`: a relative path, escaped.)
    assert_eq!(
        resolve(&base, "é://h/").unwrap().as_str(),
        "https://u:p@h:8443/a/%C3%A9://h/"
    );
    for (reference, what, at) in [
        (" x", "a reference does not start with a space", 0),
        ("x ", "a reference does not end with a space", 1),
        ("a\tb", "a URL cannot hold the control character U+0009", 1),
        ("x/%zz", "`%zz` is no escape", 2),
        ("x?a=%FF", "the escape %FF is no UTF-8 text", 4),
        ("#%FF", "the escape %FF is no UTF-8 text", 1),
        ("//", "a URL of scheme `https` has a host", 2),
        ("//h:x/", "a port cannot hold 'x'", 4),
        ("mailto:a@b", "a URL has `//` after its scheme", 7),
    ] {
        assert_eq!(
            resolve(&base, reference),
            Err(UrlError::Invalid(what.into(), at)),
            "{reference:?}"
        );
    }
}

// ── Escaping text ───────────────────────────────────────────────────

#[test]
fn escape_and_unescape_are_the_old_encoding_functions() {
    assert_eq!(escape("a b&c"), "a%20b%26c");
    assert_eq!(escape("-._~AZaz09"), "-._~AZaz09");
    assert_eq!(escape("+/?#é"), "%2B%2F%3F%23%C3%A9");
    assert_eq!(unescape("a%20b%2bc+d").unwrap(), "a b+c+d");
    assert_eq!(unescape("%C3%a9").unwrap(), "é");
    for (text, what, at) in [
        ("100%", "`%` at the end is no escape", 3),
        ("é%4", "`%4` at the end is no escape", 1),
        ("%zz", "`%zz` is no escape", 0),
        ("ab%E9", "the escape %E9 is no UTF-8 text", 2),
    ] {
        assert_eq!(
            unescape(text),
            Err(UrlError::Invalid(what.into(), at)),
            "{text}"
        );
    }
}

#[test]
fn form_bodies_are_the_old_encoding_functions() {
    assert_eq!(encode_form(&pairs(&[("q", "silt lang")])), "q=silt+lang");
    assert_eq!(
        encode_form(&pairs(&[("a&b", "c=d+e"), ("", "*-._~é")])),
        "a%26b=c%3Dd%2Be&=*-._%7E%C3%A9"
    );
    assert_eq!(encode_form(&[]), "");
    assert_eq!(
        decode_form("a=1&&b&=c&d=e=f&x=a+b%2B").unwrap(),
        pairs(&[
            ("a", "1"),
            ("b", ""),
            ("", "c"),
            ("d", "e=f"),
            ("x", "a b+")
        ])
    );
    assert_eq!(decode_form("").unwrap(), vec![]);
    assert_eq!(
        decode_form("a=1&b%zz=2"),
        Err(UrlError::Invalid("`%zz` is no escape".into(), 5))
    );
    assert_eq!(
        decode_form("é=1&b=%FF"),
        Err(UrlError::Invalid(
            "the escape %FF is no UTF-8 text".into(),
            6
        ))
    );
    let all = pairs(&[(" !\"#%&+=é", "x y")]);
    assert_eq!(decode_form(&encode_form(&all)).unwrap(), all);
}

#[test]
fn errors_never_hold_the_password() {
    let secret = "s3cretpassword";
    for text in [
        format!("http://u:{secret}@h:x/"),
        format!("http://u:{secret}@a@h/"),
        format!("http://u:{secret}%zz@h/"),
        format!("http://u:{secret}@exa mple/"),
    ] {
        let (what, _) = refusal(&text);
        assert!(!what.contains(secret), "{what}");
    }
}
