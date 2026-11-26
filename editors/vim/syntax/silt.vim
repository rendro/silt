" Vim syntax file for Silt
" Language: Silt
" Maintainer: Robert Bartel

if exists("b:current_syntax")
  finish
endif

" ── Names (generated) ──────────────────────────────────────────────
" Written from the lexer and the builtin registry by
" tests/meta/editor_grammar_names_tests.rs: do not edit by hand. After a
" change of either, `SILT_BLESS=1 cargo nextest run --test meta
" editor_grammar_names` writes these lines again.
syntax keyword siltKeyword as else fn import let match opaque pub return trait type when where
syntax keyword siltBoolean true false
" The variants of the builtin enums, one enum a line. The types come
" after them: a name that is both (json.Value's Int) is a type, as vim
" takes the keyword defined last.
syntax keyword siltConstructor Ok Err
syntax keyword siltConstructor Some None
syntax keyword siltConstructor Stop Continue
syntax keyword siltConstructor Message Empty Closed
syntax keyword siltConstructor Recv Send
syntax keyword siltConstructor Received Sent
syntax keyword siltConstructor Null Bool Int Float String Array Object
syntax keyword siltConstructor Monday Tuesday Wednesday Thursday Friday Saturday Sunday
syntax keyword siltConstructor GET POST PUT PATCH DELETE HEAD OPTIONS
syntax keyword siltConstructor Interrupt Terminate
syntax keyword siltConstructor NotFound FolderNotFound PermissionDenied AlreadyExists InvalidInput
syntax keyword siltConstructor Interrupted UnexpectedEof WriteZero Other
syntax keyword siltConstructor InvalidDigit Overflow Underflow
syntax keyword siltConstructor NotFinite
syntax keyword siltConstructor InvalidPattern TooBig
syntax keyword siltConstructor Syntax TypeMismatch MissingField
syntax keyword siltConstructor ParseFormat OutOfRange InvalidFormat
syntax keyword siltConstructor Connect Tls Timeout InvalidUrl InvalidResponse ClosedEarly
syntax keyword siltConstructor AuthFailed Query NoSuchColumn TxnAborted
syntax keyword siltConstructor InvalidUtf8 InvalidHex InvalidBase64 ByteOutOfRange
syntax keyword siltConstructor InvalidLength
syntax keyword siltConstructor Invalid InvalidPart
syntax keyword siltConstructor Bind
" The builtin types, then the record and enum types of the modules.
syntax keyword siltType Int Float Bool String List Map Set Channel Tuple Fn Bytes TcpListener
syntax keyword siltType TcpStream Regex Url
syntax keyword siltType IoError IntError FloatError ChannelResult ChannelOp Selected RuntimeError
syntax keyword siltType RegexError JsonError Value TomlError Instant Date Time DateTime Duration
syntax keyword siltType Weekday TimeError Method Response Request HttpError FileStat PostgresError
syntax keyword siltType Notification BytesError CryptoError UrlError TcpError UuidError Signal
syntax keyword siltType CsvError
" The free functions of the prelude.
syntax keyword siltBuiltin panic print println
" A builtin module's name before a dot.
syntax match siltModule "\<\(io\|string\|int\|float\|list\|map\|result\|option\|test\|channel\|task\|regex\|json\|toml\|set\|math\|time\|http\|fs\|env\|postgres\|bytes\|crypto\|url\|tcp\|stream\|uuid\|process\|csv\)\>\ze\."
" ── End of the generated names ──────────────────────────────────────────

" ── Comments ────────────────────────────────────────────────────────
syntax match siltLineComment "--.*$" contains=siltTodo
syntax region siltBlockComment start="{-" end="-}" contains=siltBlockComment,siltTodo
syntax keyword siltTodo TODO FIXME NOTE XXX HACK contained

" ── Strings ─────────────────────────────────────────────────────────
" Regular strings with interpolation
syntax region siltString start='"' skip='\\"' end='"' contains=siltStringInterp,siltStringEscape
" Escape class sourced from the lexer's escape match arms in
" src/lexer.rs (`\n \r \t \0 \\ \" \{ \}`, and `\u{...}` with 1 to 6
" hex digits). The `\{` / `\}` entries must be present so an escaped
" brace is consumed by this match and does not trigger the
" siltStringInterp region below.
" Parity lock: tests/meta/editor_grammar_string_escapes_tests.rs.
syntax match siltStringEscape '\\[nrt0\\"{}]' contained
syntax match siltStringEscape '\\u{\x\{1,6}}' contained
syntax region siltStringInterp start='{' end='}' contained contains=TOP

" Triple-quoted raw strings (no escapes, no interpolation). A run of
" quotes ends one with its last three: the ones before are text.
syntax region siltRawString start='"""' end='"""\("\)\@!'

" ── Numbers ─────────────────────────────────────────────────────────
" Mirrors the one spelling of a number in src/lexer.rs (scan_number,
" number_spelling): the prefixes `0x` and `0b`, a `_` only between two
" digits, no leading zero in a decimal number, and an exponent `e`
" with an optional `-`, no leading zero and not zero. Float patterns are defined
" AFTER the integer patterns: when two matches start at the same column
" vim prefers the item defined last, so `1.5` highlights as siltFloat
" instead of stopping at the leading `1`. Parity lock:
" tests/meta/editor_grammar_number_literals_tests.rs.
syntax match siltNumber "\<0x[0-9a-fA-F]\(_\=[0-9a-fA-F]\)*\>"
syntax match siltNumber "\<0b[01]\(_\=[01]\)*\>"
syntax match siltNumber "\<\(0\|[1-9]\(_\=\d\)*\)\>"
syntax match siltFloat "\<\(0\|[1-9]\(_\=\d\)*\)e-\=[1-9]\(_\=\d\)*\>"
syntax match siltFloat "\<\(0\|[1-9]\(_\=\d\)*\)\.\d\(_\=\d\)*\(e-\=[1-9]\(_\=\d\)*\)\=\>"

" ── Operators ───────────────────────────────────────────────────────
" Multi-character operators are listed first so vim's longest-match
" rule prefers them over the single-character forms below (e.g. `==`
" beats `=`, `->` beats `-`, `<=` beats `<`).
syntax match siltOperator "|>"
syntax match siltOperator "->"
syntax match siltOperator "\.\."
syntax match siltOperator "?"
syntax match siltOperator "\^"
syntax match siltOperator "&&"
syntax match siltOperator "||"
syntax match siltOperator "=="
syntax match siltOperator "!="
syntax match siltOperator "<="
syntax match siltOperator ">="
syntax match siltOperator "!"
" Single-character arithmetic / assignment / relational operators.
" The line-comment region (`--.*$`) and block-comment region (`{-`/`-}`)
" take priority over `match` so `-` here will not eat comment markers.
syntax match siltOperator "+"
syntax match siltOperator "-"
syntax match siltOperator "\*"
syntax match siltOperator "/"
syntax match siltOperator "%"
syntax match siltOperator "="
syntax match siltOperator "<"
syntax match siltOperator ">"

" ── Function definitions ────────────────────────────────────────────
syntax match siltFnDef "\<fn\s\+\zs\w\+"

" ── Type definitions ────────────────────────────────────────────────
" A capitalised name: a type, a trait or a variant.
syntax match siltTypeName "\<[A-Z]\w*\>"
syntax match siltTypeDef "\<type\s\+\zs\w\+"
syntax match siltTraitDef "\<trait\s\+\zs\w\+"

" ── Special syntax ──────────────────────────────────────────────────
" Collection literals
syntax match siltCollectionPrefix "#\[" " set
syntax match siltCollectionPrefix "#{"  " map

" ── Highlight links ─────────────────────────────────────────────────
highlight default link siltKeyword      Keyword
highlight default link siltBoolean      Boolean
highlight default link siltType         Type
highlight default link siltBuiltin      Function
highlight default link siltConstructor  Constant
highlight default link siltModule       Include
highlight default link siltLineComment  Comment
highlight default link siltBlockComment Comment
highlight default link siltTodo         Todo
highlight default link siltString       String
highlight default link siltRawString    String
highlight default link siltStringEscape SpecialChar
highlight default link siltStringInterp Delimiter
highlight default link siltNumber       Number
highlight default link siltFloat        Float
highlight default link siltOperator     Operator
highlight default link siltFnDef        Function
highlight default link siltTypeDef      Type
highlight default link siltTypeName     Type
highlight default link siltTraitDef     Type
highlight default link siltCollectionPrefix Delimiter

let b:current_syntax = "silt"
