# Silt for Vim

Minimal Vim integration for the Silt language: filetype detection plus syntax
highlighting. For full editor capabilities (diagnostics, hover, go-to-definition,
etc.) wire `silt lsp` into your preferred Vim LSP client — see
[`docs/editor-setup.md`](../../docs/editor-setup.md) for the canonical setup
instructions, supported features, and configuration tips.

## Layout

- `ftdetect/silt.vim` — registers the `silt` filetype for `*.silt` files.
- `syntax/silt.vim` — syntax highlighting (keywords, builtins, strings with
  `{interpolation}`, triple-quoted strings, `--` and nestable `{- -}` comments,
  numbers, operators).

## Quickstart

Drop the files into your Vim runtime path, e.g.:

```sh
mkdir -p ~/.vim/ftdetect ~/.vim/syntax
cp editors/vim/ftdetect/silt.vim ~/.vim/ftdetect/
cp editors/vim/syntax/silt.vim ~/.vim/syntax/
```

Or symlink the directories if you prefer to track this repo:

```sh
ln -s "$(pwd)/editors/vim/ftdetect/silt.vim" ~/.vim/ftdetect/silt.vim
ln -s "$(pwd)/editors/vim/syntax/silt.vim"   ~/.vim/syntax/silt.vim
```

Neovim users can use `~/.config/nvim/ftdetect/` and `~/.config/nvim/syntax/`
instead. Reload your editor and any `.silt` file will pick up the filetype and
highlighting.

For LSP-driven features (diagnostics, hover, completion, rename, formatting,
inlay hints, etc.) follow [`docs/editor-setup.md`](../../docs/editor-setup.md)
to connect `silt lsp` to your Vim LSP client of choice.
