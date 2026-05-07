# Silt for VS Code

Minimal VS Code integration for the Silt language: syntax highlighting plus an
LSP client that talks to `silt lsp`.

## Requirements

- `silt` binary on your `PATH` (or set `silt.serverPath` in settings) — build
  with `cargo build --release --features lsp` from the repo root.
- Node.js + `npm` to build the extension.

## Build locally

```sh
cd editors/vscode
npm install
npm run compile
```

## Install into VS Code

Either package it as a VSIX:

```sh
npm install -g @vscode/vsce    # once
vsce package                   # produces silt-vscode-0.1.0.vsix
code --install-extension silt-vscode-0.1.0.vsix
```

Or symlink the directory directly into your user extensions folder:

```sh
ln -s "$(pwd)" ~/.vscode/extensions/silt-lang.silt-vscode-0.1.0
```

Then reload VS Code. Opening any `.silt` file will activate the extension and
spawn `silt lsp` as the language server.

## Configuration

- `silt.serverPath` (default `silt`) — path to the silt binary.
- `silt.trace.server` — set to `messages` or `verbose` to debug LSP traffic.

## What works

Matches the capabilities advertised by `silt lsp` (see `src/lsp/mod.rs`).
Mirrors the canonical feature list in `docs/editor-setup.md`:

- Syntax highlighting (keywords, builtins, strings with `{interpolation}`,
  triple-quoted strings, `--` and nestable `{- -}` comments, numbers, operators)
- Diagnostics (lex, parse, and type errors on every edit)
- Hover (inferred types)
- Go to definition
- Go to type definition (jump from a value to the definition of its type)
- Go to implementation (jump from a trait method to its implementations)
- Find references (locate every use of a definition across the workspace)
- Rename (rename a definition and update every reference)
- Workspace symbol search (find any definition across the entire workspace by name)
- Document highlight (highlight every occurrence of the symbol under the cursor)
- Completion (keywords, stdlib, user definitions; `.` triggers member completion)
- Signature help (parameter info on `(` and `,`)
- Document symbols (outline)
- Inlay hints (inline parameter names and inferred types)
- Folding ranges (editor-driven code folding for blocks, records, and match arms)
- Selection ranges (smart syntax-aware expand/shrink selection)
- Semantic tokens (token classification for richer editor highlighting)
- Code actions (quick fixes and refactors offered for the symbol or diagnostic under the cursor)
- Formatting (via `silt fmt`)
