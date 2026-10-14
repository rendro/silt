# The playground

silt for a web page: the library built for `wasm32-unknown-unknown`,
with what a playground needs to check, run, format and colour one
program. It is what [silt-lang.com/playground](https://silt-lang.com/playground)
runs. Every release carries it as `silt-<version>-playground.tar.gz`.

## Build and test

```sh
cargo test                      # the functions, on this machine
bash build.sh                   # the module, into pkg/ (needs wasm-pack)
node tests/wasm.mjs             # the module, as a page runs it
```

## What a page calls

`pkg/silt_playground.js` is an ES module. Its default export loads
`silt_playground_bg.wasm`; after that:

| Function | Returns |
|---|---|
| `version()` | silt's version, `"0.16.0"` |
| `run(source)` | JSON `{diagnostics, output, error, ok}`: what `silt run` prints for the program, in the three parts a terminal shows one after the other |
| `check(source)` | JSON `[{from, to, severity, message}]`: what `silt check` finds |
| `format(source)` | JSON `{Ok: text}` as `silt fmt` writes it, or `{Err: report}` |
| `complete(source)` | JSON `[{label, kind}]`: the keywords, the builtins of this build, and what the text declares |
| `tokens(source)` | a `Uint32Array`: start, end and kind of each token and comment, as the lexer reads the text |
| `token_kinds()` | JSON: the names of the kinds `tokens` numbers |
| `panic_message()` | what the last panic said |

Every offset counts UTF-16 code units, as a JavaScript string does.

## What differs from `silt run`

- **One file.** The program is `main.silt`, outside any package: it
  imports builtin modules only.
- **No network, no files.** The build has none of the cargo features,
  so `http` and `tcp` do not check, and an operation on a file returns
  `Err`.
- **One thread.** A page has no other. Tasks run when the program
  waits (`docs/ffi.md`, "WebAssembly"), so what tasks print comes in an
  order of its own.
- **A sleep does not wait.** The page's clock is the time of day, and a
  sleep moves it on: a program that sleeps for an hour goes on at once,
  and reads a time one hour later from then on.
- **A program ends after 200,000,000 steps**, with a runtime error: a
  program that loops for ever would hold the page for ever.
- **A panic of silt itself is a trap.** The call throws, and the module
  is of no use afterwards: read `panic_message()`, then load the module
  again.
