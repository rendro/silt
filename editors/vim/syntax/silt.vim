" Vim syntax file for Silt
" Language: Silt
" Maintainer: Robert Bartel

if exists("b:current_syntax")
  finish
endif

" ── Keywords ────────────────────────────────────────────────────────
syntax keyword siltKeyword as else fn import let loop match pub return trait type when where
syntax keyword siltBoolean true false

" ── Primitive type names ────────────────────────────────────────────
" The variants of json.Value. Int, Float, Bool and String are types as
" well; the siltType line below is defined later, so it wins for them.
syntax keyword siltConstructor Null Bool Int Float String Array Object
" Sourced from src/types.rs `impl Display for Type` — these are the
" names the compiler prints in diagnostics and the names accepted in
" type-annotation position. Keep in sync with the lock test at
" tests/editor_grammar_primitives_tests.rs.
syntax keyword siltType Int Float Bool String Unit List Map Set Channel Tuple Fn Fun Handle Bytes TcpListener TcpStream
" The record and enum types the builtin modules declare
" (`silt::module::builtin_module_types`). Lock test at
" tests/meta/editor_grammar_primitives_tests.rs.
syntax keyword siltType FileStat Instant Date Time DateTime Duration Weekday Method Response Request
syntax keyword siltType IoError JsonError TomlError IntError FloatError HttpError RegexError TimeError BytesError PostgresError TcpError EncodingError CryptoError UuidError TaskError

" ── Builtins ────────────────────────────────────────────────────────
syntax keyword siltBuiltin print println panic
syntax keyword siltConstructor Ok Err Some None Stop Continue Message Empty Closed Recv Send Received Sent
syntax keyword siltConstructor Monday Tuesday Wednesday Thursday Friday Saturday Sunday
syntax keyword siltConstructor GET POST PUT PATCH DELETE HEAD OPTIONS
syntax keyword siltConstructor NotFound PermissionDenied AlreadyExists InvalidInput Interrupted UnexpectedEof WriteZero Other
syntax keyword siltConstructor Syntax TypeMismatch MissingField
syntax keyword siltConstructor InvalidDigit Overflow Underflow
syntax keyword siltConstructor Connect Tls Timeout InvalidUrl InvalidResponse ClosedEarly
syntax keyword siltConstructor InvalidPattern TooBig
syntax keyword siltConstructor AuthFailed Query NoSuchColumn TxnAborted
syntax keyword siltConstructor ParseFormat OutOfRange InvalidFormat
syntax keyword siltConstructor InvalidUtf8 InvalidHex InvalidBase64 ByteOutOfRange
syntax keyword siltConstructor InvalidPercent InvalidLength Invalid Bind

" ── Module names (before the dot) ───────────────────────────────────
syntax match siltModule "\<\(list\|string\|int\|float\|map\|set\|result\|option\|io\|math\|channel\|task\|regex\|json\|test\|fs\|env\|http\|time\|toml\|postgres\|bytes\|crypto\|encoding\|tcp\|stream\|uuid\)\>\ze\."

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
" Mirrors src/lexer.rs number scanning (scan_number / scan_hex_int /
" scan_binary_int): hex `0x`/`0X` and binary `0b`/`0B` prefixes, `_`
" digit separators, and e/E exponents with an optional sign and at
" least one digit immediately after the (optional) sign. Float
" patterns are defined AFTER the integer patterns: when two matches
" start at the same column vim prefers the item defined last, so
" `1.5` highlights as siltFloat instead of stopping at the leading
" `1`. Parity lock: tests/editor_grammar_number_literals_tests.rs.
syntax match siltNumber "\<0[xX][0-9a-fA-F_]\+\>"
syntax match siltNumber "\<0[bB][01_]\+\>"
syntax match siltNumber "\<\d[0-9_]*\>"
syntax match siltFloat "\<\d[0-9_]*[eE][+-]\=\d[0-9_]*\>"
syntax match siltFloat "\<\d[0-9_]*\.\d[0-9_]*\([eE][+-]\=\d[0-9_]*\)\=\>"

" ── Operators ───────────────────────────────────────────────────────
" Multi-character operators are listed first so vim's longest-match
" rule prefers them over the single-character forms below (e.g. `==`
" beats `=`, `->` beats `-`, `<=` beats `<`). Within the multi-char
" group, `...` must appear before `..` so the three-dot spread/rest
" token wins over the two-dot range token on shared prefix matches.
syntax match siltOperator "|>"
syntax match siltOperator "->"
syntax match siltOperator "\.\.\."
syntax match siltOperator "\.\."
syntax match siltOperator "::"
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
