# Link Checker Test Document

This is a test document for the Silt link checker.

## Valid Links

- [Rust homepage](https://www.rust-lang.org)
- [Silt repo](https://github.com/example/silt)
- [Local docs](/docs/getting-started.md)
- [HTTP example](http://example.com)

## Inline links

Check out [Google](https://google.com) and [Silt](https://silt-lang.org) for more info.

## Malformed Links

- [missing protocol](www.example.com)
- [bad scheme](ftp://files.example.com)
- [empty url]()
- [relative path](../some/file.txt)
- [just a word](broken)

## Edge Cases

- Not a link: just some [bracketed text] without parens
- Image link: ![alt text](https://example.com/image.png)
- [valid after bad](https://valid.example.com)
