---
title: "Editor Setup"
order: 6
description: "Configure Neovim, VS Code, and other editors to use silt's LSP server for diagnostics, hover types, completion, and formatting."
---

# Editor Setup

Silt ships with an LSP server and Vim/Neovim syntax highlighting.

## LSP Server

Start the language server with:

```sh
silt lsp
```

The server communicates over stdin/stdout using the standard LSP protocol.

### Supported Features

| Feature | Description |
|---------|-------------|
| **Diagnostics** | Lex, parse, and type errors on every edit |
| **Hover** | Show inferred type for any expression (`K` in nvim) |
| **Go to definition** | Jump to function, type, trait, let-binding definitions (`gd`) |
| **Completion** | Keywords, builtins, 160+ stdlib functions, user definitions |
| **Signature help** | Parameter names and types while typing a call |
| **Document symbols** | Outline of all declarations in the file |
| **Formatting** | Format via the existing `silt fmt` formatter |

## Neovim

### Minimal setup (built-in LSP)

Add to your `init.lua`:

```lua
-- Register .silt filetype
vim.filetype.add({ extension = { silt = 'silt' } })

-- Load syntax highlighting from silt's editors directory
vim.opt.runtimepath:append('/path/to/silt/editors/vim')

-- Start LSP on silt files
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'silt',
  callback = function()
    vim.lsp.start({
      name = 'silt',
      cmd = { 'silt', 'lsp' },
      root_dir = vim.fs.dirname(vim.fs.find({ '.git' }, { upward = true })[1]),
    })
  end,
})
```

### Recommended keymaps

```lua
vim.api.nvim_create_autocmd('LspAttach', {
  callback = function(ev)
    local opts = { buffer = ev.buf }
    vim.keymap.set('n', 'gd', vim.lsp.buf.definition, opts)
    vim.keymap.set('n', 'K', vim.lsp.buf.hover, opts)
    vim.keymap.set('n', '<leader>fm', vim.lsp.buf.format, opts)
    vim.keymap.set('n', '<leader>fs', '<cmd>Telescope lsp_document_symbols<cr>', opts)
    vim.keymap.set('n', '[d', vim.diagnostic.goto_prev, opts)
    vim.keymap.set('n', ']d', vim.diagnostic.goto_next, opts)
    vim.keymap.set('i', '<C-s>', vim.lsp.buf.signature_help, opts)
  end,
})
```

### Format on save

```lua
vim.api.nvim_create_autocmd('BufWritePre', {
  pattern = '*.silt',
  callback = function()
    vim.lsp.buf.format({ async = false })
  end,
})
```

### Completion (nvim-cmp)

Install [nvim-cmp](https://github.com/hrsh7th/nvim-cmp) with the
`cmp-nvim-lsp` source for automatic completion from the LSP.

## VS Code

Use any generic LSP client extension (e.g., "Language Server Client") with:

```json
{
  "languageServerExample.serverCommand": "silt",
  "languageServerExample.serverArgs": ["lsp"]
}
```

A dedicated VS Code extension is planned.

## Syntax Highlighting

Vim/Neovim syntax files are shipped in `editors/vim/`:

- `editors/vim/syntax/silt.vim` — full syntax highlighting
- `editors/vim/ftdetect/silt.vim` — filetype detection

Add to your runtimepath:

```lua
vim.opt.runtimepath:append('/path/to/silt/editors/vim')
```

### What's highlighted

- Keywords (`fn`, `let`, `match`, `type`, etc.)
- Builtins (`println`, `panic`, `Ok`, `Err`, `Some`, `None`, etc.)
- Module names before `.` (`list`, `string`, `map`, etc.)
- Comments (`--` line and `{- -}` block, nestable)
- Strings with `{interpolation}` and escape sequences
- Triple-quoted raw strings (`"""..."""`)
- Numbers (int and float)
- Operators (`|>`, `->`, `..`, `?`, `^`, etc.)
- Function, type, and trait names after their keyword
