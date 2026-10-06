# Golden test format (tests/golden/)

A golden case is a silt program plus the output it must produce, run
through the built `silt` binary exactly as a user would run it. No Rust.

## Layout

```
tests/golden/<area>/<case>.silt          single-file case
tests/golden/<area>/<case>.stdout        expected stdout (optional)
tests/golden/<area>/<case>.stderr        expected stderr (optional)
tests/golden/<area>/<case>.formatted     what `silt fmt` makes of the file (`-- cmd: fmt` cases)
tests/golden/<area>/<case>/main.silt     multi-file case: a directory;
tests/golden/<area>/<case>/*.silt        the other files beside it;
tests/golden/<area>/<case>/silt.toml     a package, if the case needs one
tests/golden/<area>/<case>/src/main.silt a package case: silt.toml plus src/main.silt and
                                         no main.silt; it runs as `silt run` with no
                                         file, directives are read from src/main.silt
tests/golden/<area>/<case>/case.stdout   expected output of a directory case
tests/golden/<area>/<case>/case.stderr
tests/golden/<area>/<case>/case.formatted
```

`<area>` is a directory of your choosing (e.g. `patterns`, `traits`,
`errors`, `fmt`). Case names are snake_case and say what is tested.

## Directives

Leading comment lines of the `.silt` file (for a directory case, of
`main.silt`; for a package case, of `src/main.silt`), after an optional
byte-order mark and before any code, each `-- key: value`:

| Directive | Meaning | Default |
|---|---|---|
| `-- cmd: run` / `check` / `test` / `fmt` / `fmt --check` / `disasm` / `repl` / `lsp` | the subcommand; for `repl` the file is not passed, and the session comes from `-- stdin:`; for `fmt` see "Format cases" below; for `lsp` see "LSP cases" below | `run` |
| `-- exit: N` | expected exit status | `0` |
| `-- stdout-contains: TEXT` | stdout must contain TEXT (repeatable) | — |
| `-- stderr-contains: TEXT` | stderr must contain TEXT (repeatable) | — |
| `-- stderr-not-contains: TEXT` | stderr must not contain TEXT (repeatable) | — |
| `-- stdin: TEXT` | text fed on stdin (`\n` for newlines) | empty |
| `-- repeat: N` | run N times, every run must pass (timing-sensitive cases) | 1 |
| `-- requires-feature: NAME` | skip the case unless the cargo feature is enabled (repeatable). `debug-build` is accepted as a name too: the case needs a debug build of `silt` (see "Format cases") | — |
| `-- without-feature: NAME` | skip the case when the cargo feature is enabled: what a build that lacks it does (repeatable). Such a case runs in the default-features CI job, not under `--all-features` | — |
| `-- timeout: SECONDS` | kill the case after this long (only for cases that are slow, not to hide a hang) | 20 |
| `-- verdict: same` / `known-divergent <doors>` | also run the case in verdict mode, see "Verdicts" below | — |

Comparison:
- If `<case>.stdout` exists, stdout must equal it exactly. Otherwise stdout
  is only checked by `stdout-contains`.
- Same for stderr with `<case>.stderr` and the stderr directives.
- The exit status is always checked.
- Every error diagnostic in stderr (`error[<kind>]: ...`, indented or not)
  must be followed by its ` --> ` line: every diagnostic has a place.
  The verdict mode checks the same in `check`'s stderr of every verdict
  case, the repro corpus included.
- On Windows, the backslashes in a path to a `.silt` file or to a
  package file (`silt.toml`, `silt.lock`) are turned into `/` before the
  comparison, so one expected file serves every platform: write
  `deps/x/silt.toml`, which Windows prints as `deps\x\silt.toml`.
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

## Format cases

With `-- cmd: fmt` (the command alone, without `--check`) the harness
runs `silt fmt` on the case's copy, which rewrites the copy, and then
looks at the file as well as at the output:

- A case with exit status 0 has a `<case>.formatted` file
  (`case.formatted` for a directory case, whose `main.silt` is the file
  that is formatted). The copy must equal it byte for byte, the
  directive lines included. Then `silt fmt` runs on the copy a second
  time and must change nothing: every such case is an idempotence test.
- A case with another exit status (a syntax error, a refusal) has no
  `.formatted` file: the copy must be unchanged.

`-- cmd: fmt --test-tamper=FROM=>TO` is the case of a refusal: the flag,
which `silt fmt --help` does not list, replaces the last FROM by TO in the
printer's result before the formatter checks it, as a defect of the
printer would. `silt fmt` then refuses, and the case holds what it
prints. The flag exists in a debug build only (a release binary answers
"unknown flag"), so such a case says `-- requires-feature: debug-build`
and is skipped when the suite is built with `--release`.

Stdout, stderr and the exit status are compared as for every case. To
write the expected file, format a copy and CHECK the result by eye:

```
cp <case>.silt /tmp/c.silt && $SILT fmt /tmp/c.silt && cp /tmp/c.silt <case>.formatted
```

## LSP cases

With `-- cmd: lsp` the harness starts `silt lsp` in the case's copy and
plays an editor: `initialize` with the copy as the workspace root
(`rootUri` and `workspaceFolders`), `initialized`, then a
`workspace/symbol` round trip so that whatever the server published while
loading the workspace has arrived; then `didOpen` of the entry file (the
case file, `main.silt` of a directory case, `src/main.silt` of a package
case), waits for a `publishDiagnostics` for it, does a second round trip,
and ends with `shutdown` and `exit`. A case file `<name>.unsaved` (say
`helper.silt.unsaved`) is an editor buffer: it is opened as the file
`<name>`, with its text, just before the entry file, while `<name>` on
disk keeps the saved text. A missing publish for the entry file
within the timeout fails the case; it is not taken as an empty set. So
does a server that does not exit after `exit`.

The stdout compared with `.stdout` / `-- stdout-contains` is the last
diagnostics published for each file of the case, one line each:

```
9:20 error type mismatch: expected Int, got String
util.silt:3:5 error undefined variable 'x'
```

`line:col severity message`, with a 1-based line and a 1-based column
counted in characters, as the CLI prints them (the LSP's UTF-16 positions
are converted). Lines are sorted by position; the entry file's come first
and have no file prefix, another file's are prefixed with its path
relative to the case. A message's line breaks are written `\n`. Stderr
is the server's stderr, and the exit status is the server's after `exit`
(0). `-- cmd: lsp` implies `-- requires-feature: lsp`.

`-- lsp: REQUEST` (repeatable) asks the server something about the entry
file once its diagnostics are in, in the order written. The answer is
appended to the output: the line `> REQUEST`, then one line per place,
sorted by file and position (`(nothing)` for an empty answer, `error:
MESSAGE` for an error response). Positions are `LINE:COLUMN` as above;
a range is `line:col-line:col`, its end exclusive.

| Request | Asks | A line of the answer |
|---|---|---|
| `references L:C` | `textDocument/references`, with the declaration | `geo.silt:1:8-1:10` |
| `highlight L:C` | `textDocument/documentHighlight` | `19:7-19:9` |
| `prepare-rename L:C` | `textDocument/prepareRename` | `20:13-20:19` |
| `rename L:C NAME` | `textDocument/rename` | `geo.silt:1:8-1:10 => NAME` |
| `symbols QUERY` | `workspace/symbol` (every symbol without QUERY) | `geo.silt:1:8-1:10 function mk`, a variant ends in ` in Shape` |
| `hover L:C` | `textDocument/hover` | the text, as it is |
| `format` | `textDocument/formatting`, and gives the server the result as the file's new text; the requests behind it are about that text | the new text (`(nothing)` when nothing changes) |

## Verdicts

A program's verdict is the set of static diagnostics a front door reports
for it before anything runs. The verdict mode runs a case through four
doors, `silt check`, `silt run`, `silt test` (each given the entry file)
and the LSP (as above), each in its own fresh copy, and compares their
verdicts. Runtime failures are not part of a verdict: some programs are
soundness holes that pass the static checks and trap when run.

What is compared, per door:

- Only error-severity diagnostics. Each one is keyed by (file relative to
  the case, line, column, the first line of the message, with the case's
  temporary directory written `<case>`). The human output shows no
  diagnostic codes, so the message stands in for one; the first line
  only, because the LSP's message holds the notes and help too.
- `check`, `run` and `test`: their stderr is read as rendered
  diagnostics, a header `error[<kind>]: <message>` followed by a
  ` --> file:line:col` line. `run` and `test` print their static
  diagnostics before the program starts, so reading stops at the first
  `error[runtime]` header, and the program's stdout is never read. A
  diagnostic without a location is keyed at 1:1 of the entry file, where
  the LSP puts the same location-less diagnostic. A package error
  is an `error[package]` diagnostic in a `silt.toml` or `silt.lock`,
  a dependency's included, like the others. `run` and
  `test` get empty stdin and are stopped after 10 seconds of running;
  what they printed by then is their verdict. `check` must exit with 0 or
  1 within the case's timeout, or its verdict is a failure.
- `test` on a file without `test_*` functions still analyses the whole
  file and reports its diagnostics (then "0 tests"), so it has a verdict
  like the others.
- The LSP: the error diagnostics last published for each file of the
  case (what an editor's problem list shows after opening the entry
  file). A session that publishes nothing for the entry file is a failed
  verdict.
- The entry-point diagnostics (`program has no main() function`, `the
  entry point 'main' must take no parameters`) belong to starting the
  program, so they are compared between `check` and `run` only, and left
  out when `test` and the LSP are compared with `check`.

`check` is the reference: every other door is compared with it. The mark
says how they compare:

- `-- verdict: same`: every door agrees with `check`.
- `-- verdict: known-divergent <doors>`: exactly the listed doors (`run`,
  `test`, `lsp`, in that order) disagree with `check`; the rest agree.
  This records a divergence of today's binary as the baseline.

A case fails when the doors disagree differently from its mark: a `same`
case whose doors disagree, and a `known-divergent` case whose doors now
agree (or disagree in other doors). A fix that removes a divergence
therefore fails the case until its mark is changed, so the baseline never
goes stale silently. `SILT_BLESS=1` rewrites marks along with expected
files.

The verdict cases run in their own tests, `verdict_shard_0` to
`verdict_shard_7`, separate from the `golden_shard_*` tests, which ignore
the mark. They need a build with every cargo feature (the marks are
recorded against `--all-features`) and are skipped otherwise.
`SILT_GOLDEN_SKIP_VERDICT=1` skips them too: the Windows CI jobs set it,
because each verdict case starts four processes and Windows runners are
too slow at that to fit the sample in their time cap. The verdicts do not
depend on the platform, so Linux and macOS cover them.

### The repro corpus

`repros/<area>/` holds the programs of the 2026-09-27 audit's repro
archive, imported as verdict-only cases (stage 5 decision D11): the
golden shards skip `repros/`, and of each entry file's leading comment
lines only `-- verdict:` is read. Single programs are `.silt` files; a
program that imports sibling modules is a directory case with the modules
it needs (the entry renamed `main.silt`); a package is a package case
whose path dependencies are copied under `deps/` with the manifest's
paths rewritten to them. `repros/EXCLUDED.md` lists the archive's
programs that were left out, and why.

The verdict shards run a fixed sample of 200 of them (every n-th of the
sorted list) by default. `SILT_GOLDEN_FULL_CORPUS=1` runs all of them:

```
SILT_GOLDEN_FULL_CORPUS=1 cargo test --all-features --test golden verdict_shard
```

### The soundness manifest

`repros/SOUNDNESS.tsv` is the exit test of fix-plan stage 6 (the
typechecker). It has one row for every program of `repros/type_soundness`,
`repros/typechecker_arch` and `lang/soundness`, saying what `silt check`
must report for it when stage 6 is done, whatever the binary does today.
The tests `soundness_manifest_0` to `soundness_manifest_3`
(`tests/golden/soundness.rs`) check every row with
`silt check --format json`. A row is four tab-separated fields:

```
case <TAB> state <TAB> expect <TAB> note
repros/type_soundness/g2.silt	pending:1	reject E0301	TS-1: depth two, no annotations
```

`case` is the path under `tests/golden/`. `expect` is one of:

| `expect` | The row holds when |
|---|---|
| `reject <code>` | `check` exits 1 and reports an error with this diagnostic code. Unless the code is itself a lexer or parser code, no lexer or parser error may be reported with it: a program that does not parse was not checked |
| `accept` | `check` exits 0 and reports no error |
| `output` | as `accept`, and `silt run` exits 0 and prints exactly the case's `.stdout` (`case.stdout` of a directory case): for programs that ran and printed the wrong thing |
| `obsolete` | not checked: the program's subject is gone (effects, `ExtFloat`), or the lexer or parser refuses it. The note names the port under `lang/soundness/` when the program was rewritten in today's syntax |

`state` is `holds`, or `pending:<step>` for a row that does not hold on
the current binary, with the stage 6 step expected to make it hold (`-`
for a row that is not checked). A test fails when a `holds` row does
not hold, and when a `pending` row does: the step that closes a hole
changes its rows to `holds` in the same commit, so the manifest never
goes stale. Stage 6 is done when no row is `pending`.

The code of a `pending` `reject` row is the one today's checker reports
for the same mistake written directly, or the nearest one the design note
suggests. A step whose error carries another code changes the row and
says so in its report; so does a step that moves the error of a `holds`
row to another code.

The cases under `lang/soundness/` are the repro programs that only the
parser refuses today, rewritten in current syntax (brace closures, one
statement per line). They are ordinary golden cases as well: their
directives record what the binary does now, the manifest what it must do.
Where the two differ the row is `pending` and the case says so in its
leading comment; the step that closes the hole changes both.

Like the verdict cases, the rows are checked only in a build with every
cargo feature, and not where `SILT_GOLDEN_SKIP_VERDICT=1` is set.
`SILT_GOLDEN_FILTER` narrows the rows that are run.

## Generating expected output

Run the binary on the case exactly as the harness does, and save what it
prints, then CHECK that it is what the original test asserted:

```
cd tests/golden/<area> && NO_COLOR=1 $SILT run <case>.silt > <case>.stdout 2> /tmp/err; echo $?
```

A golden captured from a wrong output is worse than no test. If the
current output contradicts what the original test asserted, do not
capture it: report it.

## Sharding

The harness runs the corpus as eight tests, `golden_shard_0` to
`golden_shard_7`, each taking every eighth case of the sorted list, so
that CI partitions stay balanced. The verdict cases are sharded the same
way, as `verdict_shard_0` to `verdict_shard_7`. A failure message names
the case.

## Bless mode

`SILT_BLESS=1 cargo test --test golden` rewrites every existing `.stdout`
/ `.stderr` / `.formatted` file and every `-- verdict:` mark from the current binary
(with `SILT_GOLDEN_FULL_CORPUS=1` for the marks of the whole repro
corpus). Only the integrator uses it,
after reviewing the diff.
