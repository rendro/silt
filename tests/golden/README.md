# Golden test format (tests/golden/)

A golden case is a silt program plus the output it must produce, run
through the built `silt` binary exactly as a user would run it. No Rust.

## Layout

```
tests/golden/<area>/<case>.silt          single-file case
tests/golden/<area>/<case>.stdout        expected stdout (optional)
tests/golden/<area>/<case>.stderr        expected stderr (optional)
tests/golden/<area>/<case>/main.silt     multi-file case: a directory;
tests/golden/<area>/<case>/*.silt        the other files beside it;
tests/golden/<area>/<case>/silt.toml     a package, if the case needs one
tests/golden/<area>/<case>/src/main.silt a package case: silt.toml plus src/main.silt and
                                         no main.silt; it runs as `silt run` with no
                                         file, directives are read from src/main.silt
tests/golden/<area>/<case>/case.stdout   expected output of a directory case
tests/golden/<area>/<case>/case.stderr
```

`<area>` is a directory of your choosing (e.g. `patterns`, `traits`,
`errors`, `fmt`). Case names are snake_case and say what is tested.

## Directives

Leading comment lines of the `.silt` file (for a directory case, of
`main.silt`; for a package case, of `src/main.silt`), after an optional
byte-order mark and before any code, each `-- key: value`:

| Directive | Meaning | Default |
|---|---|---|
| `-- cmd: run` / `check` / `test` / `fmt --check` / `disasm` / `repl` | the subcommand; for `repl` the file is not passed, and the session comes from `-- stdin:` | `run` |
| `-- exit: N` | expected exit status | `0` |
| `-- stdout-contains: TEXT` | stdout must contain TEXT (repeatable) | — |
| `-- stderr-contains: TEXT` | stderr must contain TEXT (repeatable) | — |
| `-- stderr-not-contains: TEXT` | stderr must not contain TEXT (repeatable) | — |
| `-- stdin: TEXT` | text fed on stdin (`\n` for newlines) | empty |
| `-- repeat: N` | run N times, every run must pass (timing-sensitive cases) | 1 |
| `-- timeout: SECONDS` | kill the case after this long (only for cases that are slow, not to hide a hang) | 20 |

Comparison:
- If `<case>.stdout` exists, stdout must equal it exactly. Otherwise stdout
  is only checked by `stdout-contains`.
- Same for stderr with `<case>.stderr` and the stderr directives.
- The exit status is always checked.
- A case with no expectation beyond the exit status is allowed only when
  the exit status itself is the point (say `-- exit: 0` for "this compiles
  and runs").

The harness copies the case (the file, or the whole case directory) into
a fresh temporary directory and runs the binary there, so nothing the
program or `silt` writes (a `silt.lock`, an output file) touches the
tree. A path dependency of a package case must live inside the case
directory (`dep = { path = "dep" }`). It runs the binary with that copy as the working
directory and the file name (or `main.silt`) as the argument, with
`NO_COLOR=1` and no `FORCE_COLOR`, so paths in diagnostics are relative
and stable: `--> main.silt:3:5`.

Prefer exact `.stdout` files for programs whose output is the point.
Prefer `stderr-contains` for diagnostics, naming the words that matter
(the error kind and the key phrase), so that unrelated rewording does not
break the case; use an exact `.stderr` only when the whole message is the
point (e.g. a snippet/caret layout test).

## Generating expected output

Run the binary on the case exactly as the harness does, and save what it
prints, then CHECK that it is what the original test asserted:

```
cd tests/golden/<area> && NO_COLOR=1 $SILT run <case>.silt > <case>.stdout 2> /tmp/err; echo $?
```

A golden captured from a wrong output is worse than no test. If the
current output contradicts what the original test asserted, do not
capture it: report it.

## Bless mode

`SILT_BLESS=1 cargo test --test golden` rewrites every existing `.stdout`
/ `.stderr` file from the current binary. Only the integrator uses it,
after reviewing the diff.
