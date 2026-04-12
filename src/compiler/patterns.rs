//! Pattern matching compilation for Silt.
//!
//! This module contains the pattern test, bind, and analysis methods
//! used by the compiler to emit bytecode for pattern matching constructs
//! (match arms, let-destructuring, function parameters, etc.).

use crate::ast::{Pattern, PatternKind};
use crate::bytecode::Op;
use crate::intern::{Symbol, intern, resolve};
use crate::lexer::Span;
use crate::module;
use crate::value::Value;

use super::{BindDestructKind, CompileError, Compiler};

impl Compiler {
    // ── Recursive pattern test ───────────────────────────────────
    //
    // Emit test opcodes for a pattern. The value to test is on TOS
    // (peeked, not consumed). Returns jump-patch addresses for failure.
    // For nested patterns, uses Dup + Destruct to get sub-values.

    pub(super) fn compile_pattern_test(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<Vec<usize>, CompileError> {
        match &pattern.kind {
            PatternKind::Wildcard | PatternKind::Ident(_) => {
                // Always matches, no test needed
                Ok(vec![])
            }

            PatternKind::Int(n) => {
                let idx = self.add_constant(Value::Int(*n), span)?;
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::Float(n) => {
                let idx = self.add_constant(Value::Float(*n), span)?;
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::Bool(b) => {
                self.current_chunk().emit_op(Op::TestBool, span);
                self.current_chunk().emit_u8(if *b { 1 } else { 0 }, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::StringLit(s, _) => {
                let idx = self.add_constant(Value::String(s.clone()), span)?;
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::Constructor(name, fields) => {
                // Gate constructors that require module imports
                let name_str = resolve(*name);
                if let Some(required) = module::gated_constructor_module(&name_str)
                    && !self.imported_builtin_modules.contains(required)
                {
                    return Err(CompileError {
                        message: format!("'{name}' requires `import {required}`"),
                        span,
                    });
                }
                // Test: tag matches?
                let idx = self.add_constant(Value::String(name_str), span)?;
                self.current_chunk().emit_op(Op::TestTag, span);
                self.current_chunk().emit_u16(idx, span);
                let tag_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![tag_jump];

                // Test nested field patterns
                for (i, field_pat) in fields.iter().enumerate() {
                    if !self.pattern_is_irrefutable(field_pat) {
                        // Destructure to get sub-value, test it, then pop
                        self.current_chunk().emit_op(Op::DestructVariant, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(field_pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::Tuple(pats) => {
                if pats.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "tuple pattern cannot have more than 255 elements".into(),
                        span,
                    });
                }
                // Test length
                self.current_chunk().emit_op(Op::TestTupleLen, span);
                self.current_chunk().emit_u8(pats.len() as u8, span);
                let len_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![len_jump];

                // Test nested element patterns
                for (i, pat) in pats.iter().enumerate() {
                    if !self.pattern_is_irrefutable(pat) {
                        self.current_chunk().emit_op(Op::DestructTuple, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::List(elements, rest) => {
                let elem_count = elements.len() as u8;

                if rest.is_some() {
                    // [h, ..t] — at least elem_count elements
                    self.current_chunk().emit_op(Op::TestListMin, span);
                    self.current_chunk().emit_u8(elem_count, span);
                } else {
                    // [a, b, c] — exactly elem_count elements
                    self.current_chunk().emit_op(Op::TestListExact, span);
                    self.current_chunk().emit_u8(elem_count, span);
                }
                let len_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![len_jump];

                // Test nested element patterns
                for (i, pat) in elements.iter().enumerate() {
                    if !self.pattern_is_irrefutable(pat) {
                        self.current_chunk().emit_op(Op::DestructList, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                // Test rest pattern if it's refutable
                if let Some(rest_pat) = rest
                    && !self.pattern_is_irrefutable(rest_pat)
                {
                    self.current_chunk().emit_op(Op::DestructListRest, span);
                    self.current_chunk().emit_u8(elem_count, span);
                    let sub_fails = self.compile_pattern_test(rest_pat, span)?;
                    self.current_chunk().emit_op(Op::Pop, span);
                    all_jumps.extend(sub_fails);
                }

                Ok(all_jumps)
            }

            PatternKind::Record { name, fields, .. } => {
                let mut all_jumps = Vec::new();

                // Test tag if present
                if let Some(type_name) = name {
                    let idx = self.add_constant(Value::String(resolve(*type_name)), span)?;
                    self.current_chunk().emit_op(Op::TestRecordTag, span);
                    self.current_chunk().emit_u16(idx, span);
                    let tag_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                    all_jumps.push(tag_jump);
                }

                // Test each field's sub-pattern
                for (field_name, sub_pat) in fields {
                    let sub_pattern = match sub_pat {
                        Some(p) => p,
                        None => continue, // shorthand binding {name} — always matches
                    };
                    if !self.pattern_is_irrefutable(sub_pattern) {
                        let field_idx =
                            self.add_constant(Value::String(resolve(*field_name)), span)?;
                        self.current_chunk().emit_op(Op::DestructRecordField, span);
                        self.current_chunk().emit_u16(field_idx, span);
                        let sub_fails = self.compile_pattern_test(sub_pattern, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::Range(lo, hi) => {
                let lo_idx = self.add_constant(Value::Int(*lo), span)?;
                let hi_idx = self.add_constant(Value::Int(*hi), span)?;
                self.current_chunk().emit_op(Op::TestIntRange, span);
                self.current_chunk().emit_u16(lo_idx, span);
                self.current_chunk().emit_u16(hi_idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::FloatRange(lo, hi) => {
                let lo_idx = self.add_constant(Value::Float(*lo), span)?;
                let hi_idx = self.add_constant(Value::Float(*hi), span)?;
                self.current_chunk().emit_op(Op::TestFloatRange, span);
                self.current_chunk().emit_u16(lo_idx, span);
                self.current_chunk().emit_u16(hi_idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::Or(alternatives) => {
                // Try each alternative; if any succeeds, jump to success.
                let mut fail_jumps = Vec::new();
                let mut success_jumps = Vec::new();

                for (i, alt) in alternatives.iter().enumerate() {
                    let sub_fails = self.compile_pattern_test(alt, span)?;

                    if i < alternatives.len() - 1 {
                        // Not the last alt: if it matched, jump to success
                        let success = self.current_chunk().emit_jump(Op::Jump, span);
                        success_jumps.push(success);
                        // Patch this alt's failures to try the next
                        for fj in sub_fails {
                            self.patch_jump(fj, span)?;
                        }
                    } else {
                        // Last alt: its failures are the overall failures
                        fail_jumps = sub_fails;
                    }
                }

                // Patch all success jumps to here
                for sj in success_jumps {
                    self.patch_jump(sj, span)?;
                }

                Ok(fail_jumps)
            }

            PatternKind::Pin(name) => {
                // Pin pattern: match against the existing variable's value.
                // TOS = scrutinee (peeked, not consumed).
                // Strategy: Dup scrutinee, push pin value, Eq (pops both), JumpIfFalse.
                // After: scrutinee remains on stack below the bool result.

                // Dup the scrutinee
                self.current_chunk().emit_op(Op::Dup, span);

                // Push the pin value
                if let Some(slot) = self.resolve_local(*name) {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(slot, span);
                } else if let Some(idx) = self.resolve_upvalue(*name, span)? {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    let name_idx = self.add_constant(Value::String(resolve(*name)), span)?;
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }

                // Stack: [... scrutinee, scrutinee_copy, pin_value]
                self.current_chunk().emit_op(Op::Eq, span);
                // Stack: [... scrutinee, bool_result]
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            PatternKind::Map(entries) => {
                let mut all_jumps = Vec::new();

                for (key, sub_pat) in entries {
                    // Test if key exists
                    let key_idx = self.add_constant(Value::String(key.clone()), span)?;
                    self.current_chunk().emit_op(Op::TestMapHasKey, span);
                    self.current_chunk().emit_u16(key_idx, span);
                    let key_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                    all_jumps.push(key_jump);

                    // Test sub-pattern if refutable
                    if !self.pattern_is_irrefutable(sub_pat) {
                        let key_idx2 = self.add_constant(Value::String(key.clone()), span)?;
                        self.current_chunk().emit_op(Op::DestructMapValue, span);
                        self.current_chunk().emit_u16(key_idx2, span);
                        let sub_fails = self.compile_pattern_test(sub_pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }
        }
    }

    // ── Recursive pattern bind ───────────────────────────────────
    //
    // Emit binding opcodes for a pattern after test has succeeded.
    // The value to bind FROM is on TOS.
    //
    // Contract: TOS has the value. After this call, TOS is unchanged
    // (the value is still there). New locals are pushed ABOVE it on
    // the stack via GetLocal + Destruct sequences.
    //
    // Stack layout for compound patterns like (a, b):
    //   Before: [..., tuple]
    //   After:  [..., tuple, tuple_copy(hidden), elem0, a_local,
    //                        tuple_copy2(hidden), elem1, b_local]
    // Where each GetLocal pushes a copy, Destruct pushes the element,
    // and the Ident bind dups it as the named local.

    pub(super) fn compile_pattern_bind(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            PatternKind::Ident(name) => {
                // Dup the value, the dup'd copy becomes the local's stack slot.
                self.current_chunk().emit_op(Op::Dup, span);
                // Fix B: shadow warning points at the binding's own span
                // (the `Pattern::Ident`'s span captured by the parser), not
                // at the enclosing match-arm / let statement span. This
                // lands the caret on the `result` identifier in
                // `(_, Message(result))` rather than on the `match`
                // scrutinee one line up.
                self.warn_if_shadows_module(*name, pattern.span);
                let slot = self.add_local(*name);
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(slot, span);
            }

            PatternKind::Constructor(_, fields) => {
                self.compile_compound_bind(
                    fields
                        .iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Variant(i as u8), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            PatternKind::Tuple(pats) => {
                if pats.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "tuple pattern cannot have more than 255 elements".into(),
                        span,
                    });
                }
                self.compile_compound_bind(
                    pats.iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Tuple(i as u8), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            PatternKind::List(elements, rest) => {
                if elements.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "list pattern cannot have more than 255 elements".into(),
                        span,
                    });
                }
                let mut items: Vec<(BindDestructKind, Pattern)> = elements
                    .iter()
                    .enumerate()
                    .filter_map(|(i, pat)| {
                        if self.pattern_has_bindings(pat) {
                            Some((BindDestructKind::List(i as u8), pat.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                if let Some(rest_pat) = rest
                    && self.pattern_has_bindings(rest_pat)
                {
                    items.push((
                        BindDestructKind::ListRest(elements.len() as u8),
                        (**rest_pat).clone(),
                    ));
                }
                self.compile_compound_bind(items, span)?;
            }

            PatternKind::Record { fields, .. } => {
                let mut items: Vec<(BindDestructKind, Pattern)> = Vec::new();
                for (field_name, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => {
                            if self.pattern_has_bindings(pat) {
                                items.push((
                                    BindDestructKind::RecordField(*field_name),
                                    pat.clone(),
                                ));
                            }
                        }
                        None => {
                            // Shorthand: { name } binds field to local with same name
                            items.push((
                                BindDestructKind::RecordField(*field_name),
                                Pattern::new(PatternKind::Ident(*field_name), pattern.span),
                            ));
                        }
                    }
                }
                self.compile_compound_bind(items, span)?;
            }

            PatternKind::Map(entries) => {
                let items: Vec<(BindDestructKind, Pattern)> = entries
                    .iter()
                    .filter_map(|(key, sub_pat)| {
                        if self.pattern_has_bindings(sub_pat) {
                            Some((BindDestructKind::MapValue(key.clone()), sub_pat.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                self.compile_compound_bind(items, span)?;
            }

            PatternKind::Or(alternatives) => {
                // All alternatives must bind the same variables.
                if let Some(first) = alternatives.first() {
                    let expected = Self::pattern_binding_names(first);
                    for alt in &alternatives[1..] {
                        let actual = Self::pattern_binding_names(alt);
                        if actual != expected {
                            return Err(CompileError {
                                message: "or-pattern alternatives must bind the same variables"
                                    .into(),
                                span,
                            });
                        }
                    }
                    self.compile_pattern_bind(first, span)?;
                }
            }

            // Patterns with no bindings
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Pin(_) => {
                // No bindings to create
            }
        }
        Ok(())
    }

    /// Compile bindings for a compound pattern (tuple, constructor, list, record, map).
    ///
    /// The parent value is on TOS. For each sub-pattern that has bindings,
    /// we GetLocal the parent, Destruct the sub-value, register intermediate
    /// stack values as hidden locals, and recurse.
    ///
    /// This approach "wastes" stack slots for intermediate copies but ensures
    /// local slot numbers always match actual stack positions.
    fn compile_compound_bind(
        &mut self,
        items: Vec<(BindDestructKind, Pattern)>,
        span: Span,
    ) -> Result<(), CompileError> {
        if items.is_empty() {
            return Ok(());
        }

        // The parent is on TOS. We need it in a known local slot so we
        // can GetLocal it repeatedly. We know TOS is at the "next" stack
        // position, so we can register it as a hidden local.
        // But TOS may not yet be registered. We need to check: is TOS already
        // at the expected slot position?
        //
        // Strategy: just Dup + add_local + SetLocal to get a known slot.
        // The Dup'd copy becomes a hidden local.
        self.current_chunk().emit_op(Op::Dup, span);
        let parent_slot = self.add_local(intern("__bind_parent__"));
        self.current_chunk().emit_op(Op::SetLocal, span);
        self.current_chunk().emit_u16(parent_slot, span);

        for (kind, sub_pat) in &items {
            // Push the parent value from the known slot
            self.current_chunk().emit_op(Op::GetLocal, span);
            self.current_chunk().emit_u16(parent_slot, span);

            // Destruct to get the sub-value
            match kind {
                BindDestructKind::Variant(i) => {
                    self.current_chunk().emit_op(Op::DestructVariant, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::Tuple(i) => {
                    self.current_chunk().emit_op(Op::DestructTuple, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::List(i) => {
                    self.current_chunk().emit_op(Op::DestructList, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::ListRest(start) => {
                    self.current_chunk().emit_op(Op::DestructListRest, span);
                    self.current_chunk().emit_u8(*start, span);
                }
                BindDestructKind::RecordField(name) => {
                    let field_idx = self.add_constant(Value::String(resolve(*name)), span)?;
                    self.current_chunk().emit_op(Op::DestructRecordField, span);
                    self.current_chunk().emit_u16(field_idx, span);
                }
                BindDestructKind::MapValue(key) => {
                    let key_idx = self.add_constant(Value::String(key.clone()), span)?;
                    self.current_chunk().emit_op(Op::DestructMapValue, span);
                    self.current_chunk().emit_u16(key_idx, span);
                }
            }

            // Stack: [..., parent_copy_from_GetLocal, sub_value]
            // Register the parent_copy as a hidden local
            let _copy_slot = self.add_local(intern("__destruct_copy__"));
            // Now sub_value is at the next stack position, ready for recursion.

            // Recurse into the sub-pattern for binding
            self.compile_pattern_bind(sub_pat, span)?;
        }

        Ok(())
    }

    // ── Pattern analysis helpers ─────────────────────────────────

    /// Returns true if the pattern always matches (no runtime test needed).
    pub(super) fn pattern_is_irrefutable(&self, pattern: &Pattern) -> bool {
        matches!(pattern.kind, PatternKind::Wildcard | PatternKind::Ident(_))
    }

    /// Returns true if the pattern (or any sub-pattern) binds any variable.
    pub(super) fn pattern_has_bindings(&self, pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatternKind::Ident(_) => true,
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Pin(_) => false,
            PatternKind::Constructor(_, fields) => {
                fields.iter().any(|p| self.pattern_has_bindings(p))
            }
            PatternKind::Tuple(pats) => pats.iter().any(|p| self.pattern_has_bindings(p)),
            PatternKind::List(elems, rest) => {
                elems.iter().any(|p| self.pattern_has_bindings(p))
                    || rest.as_ref().is_some_and(|r| self.pattern_has_bindings(r))
            }
            PatternKind::Record { fields, .. } => fields.iter().any(|(_, p)| {
                match p {
                    Some(pat) => self.pattern_has_bindings(pat),
                    None => true, // shorthand {name} always binds
                }
            }),
            PatternKind::Or(alts) => alts.iter().any(|p| self.pattern_has_bindings(p)),
            PatternKind::Map(entries) => {
                entries.iter().any(|(_, p)| self.pattern_has_bindings(p))
            }
        }
    }

    /// Collect the set of variable names bound by a pattern.
    fn pattern_binding_names(pattern: &Pattern) -> std::collections::BTreeSet<Symbol> {
        let mut names = std::collections::BTreeSet::new();
        Self::collect_binding_names(pattern, &mut names);
        names
    }

    fn collect_binding_names(pattern: &Pattern, names: &mut std::collections::BTreeSet<Symbol>) {
        match &pattern.kind {
            PatternKind::Ident(name) => {
                names.insert(*name);
            }
            PatternKind::Constructor(_, fields) => {
                for p in fields {
                    Self::collect_binding_names(p, names);
                }
            }
            PatternKind::Tuple(pats) => {
                for p in pats {
                    Self::collect_binding_names(p, names);
                }
            }
            PatternKind::List(elems, rest) => {
                for p in elems {
                    Self::collect_binding_names(p, names);
                }
                if let Some(r) = rest {
                    Self::collect_binding_names(r, names);
                }
            }
            PatternKind::Record { fields, .. } => {
                for (field_name, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => Self::collect_binding_names(pat, names),
                        None => {
                            names.insert(*field_name);
                        }
                    }
                }
            }
            PatternKind::Or(alts) => {
                // Collect from first alternative (all should be the same).
                if let Some(first) = alts.first() {
                    Self::collect_binding_names(first, names);
                }
            }
            PatternKind::Map(entries) => {
                for (_, p) in entries {
                    Self::collect_binding_names(p, names);
                }
            }
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Pin(_) => {}
        }
    }
}
