" Vim syntax file for Silt
" Language: Silt
" Maintainer: Robert Bartel

if exists("b:current_syntax")
  finish
endif

" ── Keywords ────────────────────────────────────────────────────────
syntax keyword siltKeyword as else fn import let loop match mod pub return trait type when where
syntax keyword siltBoolean true false

" ── Builtins ────────────────────────────────────────────────────────
syntax keyword siltBuiltin print println panic
syntax keyword siltConstructor Ok Err Some None Stop Continue Message Closed Empty

" ── Module names (before the dot) ───────────────────────────────────
syntax match siltModule "\<\(list\|string\|int\|float\|map\|set\|result\|option\|io\|math\|channel\|task\|regex\|json\|test\|fs\)\>\ze\."

" ── Comments ────────────────────────────────────────────────────────
syntax match siltLineComment "--.*$" contains=siltTodo
syntax region siltBlockComment start="{-" end="-}" contains=siltBlockComment,siltTodo
syntax keyword siltTodo TODO FIXME NOTE XXX HACK contained

" ── Strings ─────────────────────────────────────────────────────────
" Regular strings with interpolation
syntax region siltString start='"' skip='\\"' end='"' contains=siltStringInterp,siltStringEscape
syntax match siltStringEscape '\\[nrt\\"]' contained
syntax region siltStringInterp start='{' end='}' contained contains=TOP

" Triple-quoted raw strings (no escapes, no interpolation)
syntax region siltRawString start='"""' end='"""'

" ── Numbers ─────────────────────────────────────────────────────────
syntax match siltFloat "\<\d\+\.\d\+\>"
syntax match siltNumber "\<\d\+\>"

" ── Operators ───────────────────────────────────────────────────────
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

" ── Function definitions ────────────────────────────────────────────
syntax match siltFnDef "\<fn\s\+\zs\w\+"

" ── Type definitions ────────────────────────────────────────────────
syntax match siltTypeDef "\<type\s\+\zs\w\+"
syntax match siltTraitDef "\<trait\s\+\zs\w\+"

" ── Special syntax ──────────────────────────────────────────────────
" Collection literals
syntax match siltCollectionPrefix "#\[" " set
syntax match siltCollectionPrefix "#{"  " map

" ── Highlight links ─────────────────────────────────────────────────
highlight default link siltKeyword      Keyword
highlight default link siltBoolean      Boolean
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
highlight default link siltTraitDef     Type
highlight default link siltCollectionPrefix Delimiter

let b:current_syntax = "silt"
