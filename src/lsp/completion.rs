//! `textDocument/completion` handler and its dot-completion helpers.

use std::collections::HashSet;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, Documentation, MarkupContent,
    MarkupKind, Position,
};

use crate::ast::*;
use crate::intern::{intern, resolve};
use crate::lexer::{KEYWORD_LITERALS, KEYWORDS};
use crate::module;
use crate::types::Type;
use crate::types::canonical::canonical_name;

use super::Server;
use super::ast_walk::find_ident_type_by_name;
use super::conversions::position_to_offset;
use super::fields::{record_fields_from_type, type_expr_to_type};
use super::locals::locals_at_offset;
use super::state::Document;

impl Server {
    // ── Completion ─────────────────────────────────────────────────

    pub(super) fn completion(
        &self,
        params: lsp_types::CompletionParams,
    ) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri);

        // Detect dot-completion context: extract the identifier before the `.`
        if let Some(doc) = doc
            && let Some(prefix) = extract_dot_prefix(&doc.source, &pos)
        {
            let cursor = position_to_offset(&doc.source, &pos);
            // Round 81: the cached `doc.program` was parsed from the
            // user's exact source. A partial expression at the cursor
            // (`xs.|`) is a parse error that the parser's recovery
            // currently turns into an empty function-body stub — every
            // earlier `let` inside that fn vanishes from `program.decls`,
            // so `locals_at_offset` returns nothing and the
            // type-narrowing path can't find the receiver's inferred
            // type. Reparse a fix-up source where the partial dot is
            // completed with a placeholder identifier so the surrounding
            // statements (and the receiver's `let` binding) survive.
            // Falls back to the cached program when the fix-up doesn't
            // change anything (no parse error to recover from).
            let fixup_program = self.dot_completion_fixup_program(doc, &pos);
            let items = self.dot_completions(
                doc,
                fixup_program.as_ref(),
                &prefix,
                cursor,
            );
            return Some(CompletionResponse::Array(items));
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        // Keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..CompletionItem::default()
            });
        }

        // Builtins (globals + stdlib)
        for (name, kind) in builtins() {
            let detail = self.builtin_sigs.get(&name).cloned();
            let documentation = self.builtin_docs.get(&name).map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: d.clone(),
                })
            });
            items.push(CompletionItem {
                label: name,
                kind: Some(kind),
                detail,
                documentation,
                ..CompletionItem::default()
            });
        }

        // User-defined names from the current document
        if let Some(doc) = doc {
            for (name, def) in &doc.definitions {
                let kind = match &def.ty {
                    Some(Type::Fun(..)) => CompletionItemKind::FUNCTION,
                    _ => CompletionItemKind::VARIABLE,
                };
                let detail = def.ty.as_ref().map(|t| format!("{t}"));
                let documentation = def.doc.as_ref().map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(kind),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }

            // Local variables in scope at the cursor position
            if let Some(program) = &doc.program {
                let cursor = position_to_offset(&doc.source, &pos);
                for local in locals_at_offset(program, cursor) {
                    let detail = local.ty.as_ref().map(|t| format!("{t}"));
                    items.push(CompletionItem {
                        label: local.name,
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail,
                        ..CompletionItem::default()
                    });
                }
            }
        }

        Some(CompletionResponse::Array(items))
    }

    /// Produce completions after a `.` — either module functions, record
    /// fields, or methods available on the receiver's inferred type.
    ///
    /// DX-G4 (round 81): when the LSP can confidently infer the type of
    /// the receiver expression (a let-bound local with an annotation, or
    /// an identifier whose type was recovered from the typed AST), the
    /// emitted method set is narrowed to only those methods registered
    /// for that type's canonical head name. When the type cannot be
    /// inferred we fall back to the union of all method names declared
    /// in the program — preserving discoverability rather than silently
    /// hiding completions.
    fn dot_completions(
        &self,
        doc: &Document,
        fixup_program: Option<&Program>,
        prefix: &str,
        cursor: usize,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // 1. Builtin module → return its functions and constants with type signatures.
        //    Module constants (e.g. `math.pi`, `float.infinity`) are distinct from
        //    functions and must be surfaced here so editor autocompletion after
        //    `math.` / `float.` offers them alongside `sin`, `cos`, `parse`, etc.
        if module::is_builtin_module(prefix) {
            for func in module::builtin_module_functions(prefix) {
                let qualified = format!("{prefix}.{func}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                let documentation = self.builtin_docs.get(&qualified).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: func.to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }
            for constant in module::builtin_module_constants(prefix) {
                let qualified = format!("{prefix}.{constant}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                let documentation = self.builtin_docs.get(&qualified).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                items.push(CompletionItem {
                    label: constant.to_string(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    detail,
                    documentation,
                    ..CompletionItem::default()
                });
            }
            // Gated enum constructors that belong to this module
            // (e.g. `io.IoNotFound`, `http.GET`, `channel.Recv`,
            // `time.Monday`, `postgres.PgConnect`). Emitted as
            // CONSTRUCTOR entries so editors distinguish them from
            // module functions / constants.
            //
            // Silt does not expose gated constructors via `module.Name`
            // at the runtime binding level — they bind as bare globals
            // once the module is imported — but surfacing them after a
            // `.` is the most intuitive discovery affordance for users
            // exploring the API.
            for (_enum_name, variants) in module::builtin_enum_variants() {
                for &variant in *variants {
                    if module::gated_constructor_module(variant) == Some(prefix) {
                        items.push(CompletionItem {
                            label: variant.to_string(),
                            kind: Some(CompletionItemKind::CONSTRUCTOR),
                            ..CompletionItem::default()
                        });
                    }
                }
            }
            // Deterministic ordering so clients/tests see a stable list, and
            // dedupe in case a name was declared as both function and constant.
            items.sort_by(|a, b| a.label.cmp(&b.label));
            items.dedup_by(|a, b| a.label == b.label);
            return items;
        }

        let program = match &doc.program {
            Some(p) => p,
            None => return items,
        };

        // Resolve the receiver's inferred type once. Used both for the
        // (existing) record-field path and the (new, round 81 DX-G4)
        // method-narrowing path below. Two narrow sources are tried:
        //
        //   (a) a let-bound local in scope at the cursor whose type was
        //       recovered from the value expression (annotation included);
        //   (b) any typed-AST occurrence of `prefix` as an identifier
        //       whose `expr.ty` survived inference without unresolved
        //       variables.
        //
        // Anything else (complex expressions, generics with unresolved
        // tyvars, non-identifier prefixes) leaves `receiver_ty = None`
        // and the method path falls back to the full registry — the
        // task's "narrow when safe, never silently lose completions"
        // contract.
        //
        // When a `fixup_program` is supplied (the common case in real
        // editor use: the user is mid-edit at `xs.|`), prefer it for
        // the local-binding scan. The cached `doc.program` was parsed
        // from a source that already had the partial dot expression and
        // the parser's recovery may have collapsed the entire enclosing
        // function body to an empty stub, hiding `xs`. The fix-up
        // program reparses with the partial dot completed by a
        // placeholder so the prior `let xs: ...` survives.
        let lookup_program = fixup_program.unwrap_or(program);
        let locals = locals_at_offset(lookup_program, cursor);
        let receiver_ty: Option<Type> = locals
            .iter()
            .rev()
            .find(|l| l.name == prefix)
            .and_then(|l| l.ty.clone())
            .or_else(|| find_ident_type_by_name(lookup_program, prefix));

        // 2. Record fields, if the receiver is a record-shaped type.
        let mut emitted_field_labels: HashSet<String> = HashSet::new();
        if let Some(ref ty) = receiver_ty
            && let Some(fields) = record_fields_from_type(ty, lookup_program)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
                emitted_field_labels.insert(name.clone());
            }
        }

        // 3. Fallback for type-name prefix (`Point.|`): offer its fields
        //    even when the prefix is not a value binding. Same behaviour
        //    as before — guarded by `emitted_field_labels` so we never
        //    duplicate a field already surfaced via the value path.
        if receiver_ty.is_none() {
            let prefix_sym = intern(prefix);
            for decl in &lookup_program.decls {
                if let Decl::Type(td) = decl
                    && td.name == prefix_sym
                    && let TypeBody::Record(fields) = &td.body
                {
                    for field in fields {
                        let label = field.name.to_string();
                        if emitted_field_labels.insert(label.clone()) {
                            items.push(CompletionItem {
                                label,
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(format!("{}", type_expr_to_type(&field.ty))),
                                ..CompletionItem::default()
                            });
                        }
                    }
                }
            }
        }

        // 4. Method completions (round 81 DX-G4).
        //
        // When the receiver type is known, narrow to methods declared
        // for that type's canonical head name (so `xs: List(Int)`
        // surfaces only List methods, not String methods). When the
        // type is unknown, emit the union of every method name declared
        // anywhere in the program — discoverable but unfiltered.
        //
        // Use `lookup_program` here (the fix-up program when supplied)
        // so a partial dot expression doesn't collapse the enclosing
        // function and hide every TraitImpl declared after it. Falls
        // back to the cached `program` when no fix-up was needed.
        let methods = methods_for_receiver(lookup_program, receiver_ty.as_ref());
        for label in methods {
            // Don't duplicate a name that already came through as a
            // field — fields and methods occupy the same `name.` slot.
            if emitted_field_labels.contains(&label) {
                continue;
            }
            items.push(CompletionItem {
                label,
                kind: Some(CompletionItemKind::METHOD),
                ..CompletionItem::default()
            });
        }

        items.sort_by(|a, b| a.label.cmp(&b.label));
        items.dedup_by(|a, b| a.label == b.label);
        items
    }

    /// Reparse the document with the partial dot expression at `pos`
    /// completed by a placeholder identifier so the surrounding
    /// statements survive parser recovery.
    ///
    /// Background (round 81 DX-G4): the cached `doc.program` was
    /// produced by parsing the user's exact source. When the cursor
    /// sits at a partial `xs.|`, the parser's `expect_ident()` after
    /// the `.` errors out, and `parse_let_stmt`'s `?` propagates the
    /// failure all the way to `parse_fn_decl_recovering`, which
    /// salvages a *recovery stub* for the enclosing function — an
    /// `FnDecl` with an empty body. Every prior `let` in that
    /// function disappears from the AST, which means `locals_at_offset`
    /// returns nothing for the receiver `xs` and the type-narrowing
    /// path can't pin down its type.
    ///
    /// The fix-up reparses a one-token-edited copy of the source where
    /// the partial dot is followed by a placeholder identifier
    /// (`silt_lsp_completion_placeholder`). This makes the surrounding
    /// `let _ = xs.silt_lsp_completion_placeholder` parse as a normal
    /// FieldAccess (which the typechecker will reject with an "unknown
    /// field" diagnostic, but that diagnostic is harmless inside this
    /// throwaway program — we only consume the AST shape, not the
    /// errors). The receiver's `let xs: List(Int) = ...` survives
    /// inside the function body, and `locals_at_offset` returns it
    /// with its inferred type.
    ///
    /// Returns `None` if there's no `.` immediately before the cursor
    /// (so the standard non-fix-up path runs) or if reparsing somehow
    /// fails to produce a usable Program (defensive).
    fn dot_completion_fixup_program(
        &self,
        doc: &Document,
        pos: &Position,
    ) -> Option<Program> {
        let cursor = position_to_offset(&doc.source, pos);
        // Sanity: the byte just before the cursor must be `.`. If not,
        // the dot-completion context was extracted from a different
        // configuration (e.g. `xs.first().` chained-call walk reached
        // back through a `)`) and the fix-up isn't needed.
        if cursor == 0 || !doc.source.is_char_boundary(cursor) {
            return None;
        }
        let bytes = doc.source.as_bytes();
        if bytes.get(cursor.checked_sub(1)?) != Some(&b'.') {
            return None;
        }
        // Build the fix-up source: insert a placeholder identifier
        // right after the cursor (which sits one past the `.`). The
        // placeholder is intentionally long-and-prefixed so it can't
        // accidentally collide with a real user method name.
        const PLACEHOLDER: &str = "silt_lsp_completion_placeholder";
        let mut fixed = String::with_capacity(doc.source.len() + PLACEHOLDER.len());
        fixed.push_str(&doc.source[..cursor]);
        fixed.push_str(PLACEHOLDER);
        fixed.push_str(&doc.source[cursor..]);

        // Lex / parse / typecheck the fix-up source. We discard parse
        // and typecheck errors — the goal is "AST that locates the
        // receiver's binding," not "clean diagnostics."
        let tokens = match crate::lexer::Lexer::new(&fixed).tokenize() {
            Ok(t) => t,
            Err(_) => return None,
        };
        let (mut program, _parse_errs) =
            crate::parser::Parser::new(tokens).parse_program_recovering();
        let _type_errs = crate::typechecker::check(&mut program);
        Some(program)
    }
}

// ── Method enumeration for dot-completion ──────────────────────────

/// Auto-derived built-in trait method names for primitive + container
/// types. Mirrors `register_auto_derived_impls_for` in
/// `src/typechecker/mod.rs::register_builtin_trait_impls`. Each entry
/// maps the canonical type-name string (matching `canonical_name(&ty)`
/// in `src/types/canonical.rs`) to the methods that the auto-derive
/// pass stamps for that type.
///
/// These methods are never written into `program.decls` as `TraitImpl`
/// nodes — the typechecker registers them directly into `method_table`
/// via `register_auto_derived_impls_for`. The LSP cannot reach
/// `method_table` (it's owned by the typechecker and dropped after
/// `check`), so the canonical knowledge is mirrored here.
///
/// Non-primitive types (user records / enums and built-in enums like
/// `Option`, `Result`, `Weekday`) DO get auto-derive impls synthesized
/// into the AST (see `synthesize_auto_derive_impls`), so they flow
/// through `program.decls` naturally — no entry needed here.
fn auto_derived_methods_for(canon_name: &str) -> &'static [&'static str] {
    // Source: src/typechecker/mod.rs::register_builtin_trait_impls
    //   - Int/Float/ExtFloat/Bool/String/Unit + List → all four traits
    //     (Equal, Compare, Hash, Display).
    //   - Tuple/Map/Set → Equal/Hash/Display only (no Compare).
    //
    // Trait method names are sourced from `builtin_trait_decls`
    // (src/typechecker/mod.rs:6926):
    //   Display → display, Compare → compare, Equal → equal, Hash → hash.
    //
    // `Bytes` is a stdlib alias for `List(Int)` and thus collapses onto
    // `"List"` via `canonicalize_type_name`; see `register_auto_derived
    // _impls_for(checker, &["Bytes"], &["Display"])`. Display is the
    // only trait registered for the bytes-specific stamp; the rest
    // route through List's entry below.
    match canon_name {
        "Int" | "Float" | "ExtFloat" | "Bool" | "String" | "Unit" | "List" => {
            &["display", "compare", "equal", "hash"]
        }
        "Tuple" | "Map" | "Set" => &["display", "equal", "hash"],
        _ => &[],
    }
}

/// Return the method names available on a value whose canonical type
/// name is `canon_name`. Walks `program.decls` for `TraitImpl` entries
/// targeting the canonical name (both user-written and synthesized
/// auto-derive impls land in `program.decls`), then unions in any
/// auto-derived built-in trait methods that aren't carried by the AST
/// (primitives / containers — see `auto_derived_methods_for`).
fn methods_for_canon_name(program: &Program, canon_name: &str) -> Vec<String> {
    let mut out: HashSet<String> = HashSet::new();
    for decl in &program.decls {
        if let Decl::TraitImpl(ti) = decl {
            // Compare against the canonical form of the impl's target
            // (so `Range -> List` and `() -> Unit` collapses match
            // expectations the same way the typechecker routes them at
            // dispatch time). Note: `Bytes` is NOT canonicalized here;
            // user aliases such as `type Bytes = List(Int)` are missed
            // by this LSP-side mirror — see `canonicalize_target_name`
            // at the function below for the exact set of rewrites.
            let target_name = resolve(ti.target_type);
            let target_canon = canonicalize_target_name(&target_name);
            if target_canon == canon_name {
                for m in &ti.methods {
                    out.insert(resolve(m.name).to_string());
                }
            }
        }
    }
    for m in auto_derived_methods_for(canon_name) {
        out.insert((*m).to_string());
    }
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

/// Mirror of the canonicalization rules in
/// `src/types/canonical.rs::canonicalize_type_name` for the cases the
/// LSP needs to recognise. Keeps the LSP side in lock-step with the
/// typechecker's dispatch-key reduction without dragging in the full
/// `Resolver` (user aliases like `type Bytes = List(Int)` ARE missed
/// here — handling those would require routing the LSP through the
/// typechecker's `Resolver`, which is out of scope for round 81's
/// minimum-viable narrowing pass).
fn canonicalize_target_name(name: &str) -> &str {
    match name {
        "Range" => "List",
        "Fun" => "Fn",
        "()" => "Unit",
        other => other,
    }
}

/// Union of every method name declared anywhere in the program plus
/// every auto-derived built-in trait method name. Used as the fallback
/// when the receiver's type cannot be confidently inferred — preserves
/// the contract that narrowing only ever reduces noise, never silently
/// hides a completion.
fn all_known_method_names(program: &Program) -> Vec<String> {
    let mut out: HashSet<String> = HashSet::new();
    for decl in &program.decls {
        if let Decl::TraitImpl(ti) = decl {
            for m in &ti.methods {
                out.insert(resolve(m.name).to_string());
            }
        }
    }
    // Always include the auto-derived built-in trait methods. They are
    // never written into the AST for primitives, but a user might be
    // typing on any of the primitive heads, so include them here.
    for m in &["display", "compare", "equal", "hash", "message"] {
        out.insert((*m).to_string());
    }
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

/// Dispatch shim: returns the narrowed method set when `receiver_ty`
/// is `Some(_)`, or the full union when it is `None`.
fn methods_for_receiver(program: &Program, receiver_ty: Option<&Type>) -> Vec<String> {
    match receiver_ty {
        Some(ty) => {
            let canon = canonical_name(ty);
            // `_` / `<anon>` / `Never` are placeholder canonical names
            // emitted for unresolved variables, anonymous records, and
            // bottom-typed expressions (see canonical.rs:583-602). None
            // of them should narrow — they signal "type unknown / no
            // dispatch head", so we fall back to the full set rather
            // than emitting an empty completion list.
            if canon == "_" || canon == "<anon>" || canon == "Never" {
                all_known_method_names(program)
            } else {
                methods_for_canon_name(program, &canon)
            }
        }
        None => all_known_method_names(program),
    }
}

// ── Dot-completion helpers ─────────────────────────────────────────

/// Extract the identifier (or postfix-call / index-expression receiver)
/// before the `.` at the cursor position.
///
/// Returns `None` if the cursor is not in a dot-completion context.
///
/// The walk handles chained method calls and index expressions — `xs.first().`
/// and `arr[0].` must trigger completion on the receiver, not bail because
/// `)` / `]` terminate the identifier scan. We scan char-by-char from right
/// to left, skipping over matched `()` / `[]` spans, then greedily collect
/// identifier characters (plus `.` for qualified names like `mod.inner.`).
/// Anything else terminates the walk — keeps multi-statement lines from
/// greedily swallowing the previous expression.
fn extract_dot_prefix(source: &str, pos: &Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    if col == 0 {
        return None;
    }
    // Convert UTF-16 offset to byte offset
    let mut utf16_offset = 0usize;
    let mut byte_offset = line.len();
    for (byte_idx, ch) in line.char_indices() {
        if utf16_offset >= col {
            byte_offset = byte_idx;
            break;
        }
        utf16_offset += ch.len_utf16();
    }
    let before = &line[..byte_offset];
    // The last character should be '.' (cursor is right after it)
    if !before.ends_with('.') {
        return None;
    }
    let before_dot = &before[..before.len() - 1];
    // Walk backwards, skipping balanced `()` / `[]` groups so method
    // chains like `xs.first().` and index expressions like `arr[0].`
    // resolve to a sensible receiver instead of bailing on the closer.
    let chars: Vec<char> = before_dot.chars().collect();
    let mut end = chars.len(); // exclusive upper bound of the prefix
    loop {
        if end == 0 {
            break;
        }
        let last = chars[end - 1];
        match last {
            ')' | ']' => {
                let (open, close) = if last == ')' { ('(', ')') } else { ('[', ']') };
                // Scan back to the matching opener, handling nesting.
                let mut depth = 1i32;
                let mut i = end - 1; // index of the closer
                while i > 0 {
                    i -= 1;
                    let c = chars[i];
                    if c == close {
                        depth += 1;
                    } else if c == open {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                if depth != 0 {
                    // Unbalanced — stop here.
                    break;
                }
                // `i` now points at the matching opener; consume through it.
                end = i;
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => {
                // Greedily consume the identifier (possibly qualified).
                while end > 0 {
                    let ch = chars[end - 1];
                    if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                        end -= 1;
                    } else {
                        break;
                    }
                }
                break;
            }
            _ => break,
        }
    }
    let prefix: String = chars[end..].iter().collect();
    // Strip a trailing `.` — shouldn't happen in practice but keeps the
    // contract "returned prefix never ends with `.`" trivially true.
    let prefix = prefix.trim_end_matches('.').to_string();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

// ── Completion data ────────────────────────────────────────────────

// `KEYWORDS` is sourced from `crate::lexer::KEYWORDS` — the authoritative
// list maintained alongside the lexer's keyword match arms. Re-introducing
// a hand-rolled list here is guarded by `tests/lexer_keyword_parity_tests.rs`.

/// Build the builtins completion list dynamically from the module registry
/// so it never falls out of sync with `module.rs`.
///
/// `pub` so integration tests (see `tests/builtin_constructor_parity_tests.rs`)
/// can assert every gated constructor from
/// `module::all_builtin_constructor_names` is emitted here.
pub fn builtins() -> Vec<(String, CompletionItemKind)> {
    // Globals (not part of any module). Sourced from
    // `module::builtin_free_function_names()` so adding a new free
    // function (e.g. `eprintln`, `assert`) flows through automatically.
    // Parity lock: `tests/builtin_free_function_parity_tests.rs`.
    let mut items: Vec<(String, CompletionItemKind)> = module::builtin_free_function_names()
        .iter()
        .map(|name| ((*name).to_string(), CompletionItemKind::FUNCTION))
        .collect();

    // Reserved-word-shaped boolean literals. Sourced from
    // `crate::lexer::KEYWORD_LITERALS` so additions there flow through
    // automatically — mirrors the round-63 pattern used for `KEYWORDS`
    // above and the round-64 G4 fix in `src/repl.rs`. Parity lock:
    // `tests/lexer_keyword_parity_tests.rs`.
    for kw in KEYWORD_LITERALS {
        items.push((kw.to_string(), CompletionItemKind::CONSTANT));
    }

    // Round-62 G6: primitive + container type names from the
    // authoritative `BUILTIN_TYPES` constant. Surfaced as completion
    // items so an editor offers `Int`, `Bool`, `List`, etc. wherever
    // identifier completion runs (notably in type-annotation positions
    // like `let x: B|`). Derived from
    // `crate::types::builtins::iter_all` so additions flow through.
    for entry in crate::types::builtins::iter_all() {
        // Skip the surface-alias `()` — completion items must be valid
        // identifiers users would type to commit.
        if entry.name == "()" {
            continue;
        }
        items.push((entry.name.to_string(), CompletionItemKind::CLASS));
    }

    // Every builtin enum constructor — prelude (Ok/Err/Some/None) plus
    // every gated variant (Recv/Send, IoNotFound, PgConnect, Monday,
    // GET/POST/…, etc.). Sourced from the authoritative module helper
    // so new variants flow through without editing this list.
    for name in module::all_builtin_constructor_names() {
        items.push((name.to_string(), CompletionItemKind::CONSTRUCTOR));
    }

    for &m in module::BUILTIN_MODULES {
        for func in module::builtin_module_functions(m) {
            items.push((format!("{m}.{func}"), CompletionItemKind::FUNCTION));
        }
        for constant in module::builtin_module_constants(m) {
            items.push((format!("{m}.{constant}"), CompletionItemKind::CONSTANT));
        }
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_check(source: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut prog, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut prog);
        prog
    }

    /// `methods_for_canon_name` filters TraitImpl methods by the
    /// receiver's canonical type name. With the source declaring two
    /// user impls — one on String (`UpperCaser.to_upper`) and one on
    /// List (`IntListHead.head_int`) — querying for "List" must return
    /// the List-impl's method plus the auto-derived built-in trait
    /// methods, and must NOT leak the String-impl's method.
    #[test]
    fn methods_for_canon_name_filters_by_target_type() {
        let source = "trait UpperCaser { fn to_upper(self) -> String }\n\
                      trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                      trait IntListHead { fn head_int(self) -> Int }\n\
                      trait IntListHead for List { fn head_int(self) -> Int { 0 } }\n\
                      fn main() { 0 }\n";
        let prog = parse_check(source);
        let m_list = methods_for_canon_name(&prog, "List");
        assert!(m_list.contains(&"head_int".to_string()), "got: {m_list:?}");
        assert!(m_list.contains(&"display".to_string()), "got: {m_list:?}");
        assert!(!m_list.contains(&"to_upper".to_string()), "got: {m_list:?}");

        let m_string = methods_for_canon_name(&prog, "String");
        assert!(m_string.contains(&"to_upper".to_string()), "got: {m_string:?}");
        assert!(m_string.contains(&"display".to_string()), "got: {m_string:?}");
        assert!(!m_string.contains(&"head_int".to_string()), "got: {m_string:?}");
    }

    /// `all_known_method_names` is the fallback path's source — every
    /// method name declared anywhere in the program plus the auto-
    /// derived built-in trait method names. Asserts both are present
    /// so the contract "narrowing only ever reduces noise, never hides
    /// completions on an unknown receiver" is observably maintained.
    #[test]
    fn all_known_method_names_unions_user_and_builtin() {
        let source = "trait UpperCaser { fn to_upper(self) -> String }\n\
                      trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                      fn main() { 0 }\n";
        let prog = parse_check(source);
        let all = all_known_method_names(&prog);
        // User-declared impl method.
        assert!(all.contains(&"to_upper".to_string()), "got: {all:?}");
        // Auto-derived built-in trait methods. `message` is part of the
        // fallback list at the call site (src/lsp/completion.rs ~512)
        // because the `Error` builtin trait exposes a `message` method;
        // earlier rounds asserted only `display`/`compare`/`equal`/`hash`
        // and dropped `message` silently when the trait registry was
        // out of sync. Lock it explicitly so any future rename or
        // accidental deletion fails this test.
        assert!(all.contains(&"display".to_string()), "got: {all:?}");
        assert!(all.contains(&"compare".to_string()), "got: {all:?}");
        assert!(all.contains(&"equal".to_string()), "got: {all:?}");
        assert!(all.contains(&"hash".to_string()), "got: {all:?}");
        assert!(all.contains(&"message".to_string()), "got: {all:?}");
    }

    /// `canonicalize_target_name` mirrors the typechecker's reduction
    /// rules used when keying `method_table` entries. Range collapses
    /// to List, Fun to Fn, () to Unit; everything else round-trips.
    #[test]
    fn canonicalize_target_name_mirrors_typechecker_rules() {
        assert_eq!(canonicalize_target_name("Range"), "List");
        assert_eq!(canonicalize_target_name("Fun"), "Fn");
        assert_eq!(canonicalize_target_name("()"), "Unit");
        assert_eq!(canonicalize_target_name("List"), "List");
        assert_eq!(canonicalize_target_name("MyType"), "MyType");
    }
}
