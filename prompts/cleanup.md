# Silt Cleanup Prompt

Use this prompt to remove all generated evaluation/friction artifacts and restore the repo to a clean state.

---

Remove all evaluation report markdown files and friction analysis programs from the silt repo. Preserve core documentation and examples.

## What to delete

### Report files (root)
```
rm -f EVALUATION.md
rm -f ROADMAP.md
```

### Friction report
```
rm -f docs/friction-report.md
```

### All friction programs and their data files
```
rm -rf programs/
```

## What to keep

### Core documentation (do NOT delete)
```
docs/README.md
docs/concurrency.md
docs/design-decisions.md
docs/getting-started.md
docs/language-guide.md
docs/stdlib-reference.md
language-spec.md
```

### Examples (do NOT delete)
```
examples/
```

### All source code and tests (do NOT delete)
```
src/
tests/
Cargo.toml
Cargo.lock
.gitignore
```

### Prompts (do NOT delete)
```
prompts/
```

## Verification

After cleanup, verify the repo is in a clean state:

```bash
# Should show no .md files in root except language-spec.md
ls *.md

# Should show no programs/ directory
ls programs/ 2>/dev/null && echo "ERROR: programs/ still exists" || echo "OK: programs/ removed"

# Should show docs/ with only core files (no friction-report.md)
ls docs/

# Source and tests should be untouched
ls src/*.rs
ls tests/*.rs

# Tests should still pass
~/.cargo/bin/cargo test --manifest-path /home/klaus/dev/silt/Cargo.toml
```

## When to use this

Run this cleanup before:
- Starting a fresh friction analysis (so agents don't see prior programs)
- Starting a fresh evaluation (so the evaluator doesn't find stale reports)
- Preparing a clean commit of language changes without evaluation artifacts

## One-liner version

If you just want the commands without the ceremony:

```bash
cd /home/klaus/dev/silt
rm -f EVALUATION.md ROADMAP.md docs/friction-report.md
rm -rf programs/
```
