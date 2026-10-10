//! Pattern matching compilation for Silt.
//!
//! This module contains the pattern test, bind, and analysis methods
//! used by the compiler to emit bytecode for pattern matching constructs
//! (match arms, let-destructuring, function parameters, etc.).

use crate::ast::{Pattern, PatternKind};
use crate::bytecode::{Asm, Label};
use crate::intern::{Symbol, intern, resolve};
use crate::source::Span;
use crate::value::Value;

use super::{BindDestructKind, Compiler, FieldRead, name_without_binding};
use crate::diagnostic::{Code, Diagnostic};

impl Compiler {
    /// The declared record type the record pattern `pattern` names, if
    /// it names one (`Pt { x }`).
    fn pattern_record_type(
        &self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<Option<std::sync::Arc<crate::typeinfo::TypeInfo>>, Diagnostic> {
        match &pattern.kind {
            PatternKind::Record {
                name: Some(name), ..
            } => self.record_type(pattern.res, *name, span).map(Some),
            _ => Ok(None),
        }
    }

    /// Emit the shape test of a tuple pattern with `len` elements for the
    /// value on TOS and return the failure jump. The pattern `()` has no
    /// elements and matches the unit value, which is not a tuple at run
    /// time. Shared by both pattern-test compilers below.
    fn emit_tuple_shape_test(&mut self, len: usize, span: Span) -> Result<Label, Diagnostic> {
        if len == 0 {
            let unit = self.add_constant(Value::Unit, span)?;
            self.emit(Asm::TestEqual { k: unit }, span)?;
        } else {
            self.emit(Asm::TestTupleLen { len }, span)?;
        }
        self.jump_if_false(span)
    }

    // ── Recursive pattern test ───────────────────────────────────
    //
    // Emit test opcodes for a pattern. The value to test is on TOS
    // (peeked, not consumed). Returns the jumps taken when the test
    // fails. For nested patterns, uses Destruct to get sub-values; a
    // failed test of a nested pattern leaves the sub-values it was
    // looking at above the value.
    //
    // A pattern the typechecker marked irrefutable matches every value
    // of its type: no test is emitted for it, whatever it names (a
    // variant, a record type, a tuple).

    pub(super) fn compile_pattern_test(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<Vec<Label>, Diagnostic> {
        let fails = self.compile_pattern_test_tracked(pattern, span, 0)?;
        Ok(fails.into_iter().map(|(jump, _)| jump).collect())
    }

    // `compile_pattern_test`, with the number of sub-values each failed
    // test leaves above the value: `(jump, depth)` pairs. An or-pattern
    // needs the depths, to drop the sub-values of a failed alternative
    // before the next one is tested.
    //
    // `base_depth` is the number of sub-values already above the value
    // from an outer compound pattern. Each Destruct in a compound
    // pattern increments the depth; each Pop decrements it.

    fn compile_pattern_test_tracked(
        &mut self,
        pattern: &Pattern,
        span: Span,
        base_depth: usize,
    ) -> Result<Vec<(Label, usize)>, Diagnostic> {
        if pattern.irrefutable {
            return Ok(vec![]);
        }
        match &pattern.kind {
            // ── Simple (leaf) patterns ──────────────────────────
            // These never push intermediate Destruct values, so the
            // depth is unchanged from the base.
            PatternKind::Wildcard | PatternKind::Ident(_) => Ok(vec![]),

            PatternKind::Int(n) => {
                let idx = self.add_constant(Value::Int(*n), span)?;
                self.emit(Asm::TestEqual { k: idx }, span)?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::Float(n) => {
                let idx = self.add_constant(Value::Float(*n), span)?;
                self.emit(Asm::TestEqual { k: idx }, span)?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::Bool(b) => {
                let idx = self.add_constant(Value::Bool(*b), span)?;
                self.emit(Asm::TestEqual { k: idx }, span)?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::StringLit(s) => {
                let idx = self.add_constant(Value::String(s.clone().into()), span)?;
                self.emit(Asm::TestEqual { k: idx }, span)?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::Range(lo, hi) => {
                let lo_idx = self.add_constant(Value::Int(*lo), span)?;
                let hi_idx = self.add_constant(Value::Int(*hi), span)?;
                self.emit(
                    Asm::TestIntRange {
                        lo: lo_idx,
                        hi: hi_idx,
                    },
                    span,
                )?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::FloatRange(lo, hi) => {
                let lo_idx = self.add_constant(Value::Float(*lo), span)?;
                let hi_idx = self.add_constant(Value::Float(*hi), span)?;
                self.emit(
                    Asm::TestFloatRange {
                        lo: lo_idx,
                        hi: hi_idx,
                    },
                    span,
                )?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            PatternKind::Pin(name) => {
                // The name is one from outside the pattern, also where
                // the pattern is tested again while its names are bound
                // (the alternatives of an or-pattern).
                self.emit(Asm::Dup, span)?;
                if let Some(slot) = self.resolve_local_outside_pattern(*name) {
                    self.emit(Asm::GetLocal { slot }, span)?;
                } else if let Some(idx) = self.resolve_upvalue(*name) {
                    self.emit(Asm::GetUpvalue { index: idx }, span)?;
                } else if let Some(def) = self.value_def(pattern.res) {
                    self.emit_global_value(def, span)?;
                } else {
                    return Err(name_without_binding(span, *name));
                }
                self.emit(Asm::Eq, span)?;
                let jump = self.jump_if_false(span)?;
                Ok(vec![(jump, base_depth)])
            }

            // ── Compound patterns ──────────────────────────────
            // These push intermediate Destruct values on the stack.
            PatternKind::Constructor {
                name, args: fields, ..
            } => {
                let tag = self.pattern_tag(pattern.res, *name, span)?;
                let idx = self.add_constant(Value::VariantConstructor(tag), span)?;
                self.emit(Asm::TestTag { tag: idx }, span)?;
                let tag_jump = self.jump_if_false(span)?;
                let mut all_jumps = vec![(tag_jump, base_depth)];

                for (i, field_pat) in fields.iter().enumerate() {
                    if !field_pat.irrefutable {
                        self.emit(Asm::DestructVariant { index: i }, span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(field_pat, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::Tuple(pats) => {
                let len_jump = self.emit_tuple_shape_test(pats.len(), span)?;
                let mut all_jumps = vec![(len_jump, base_depth)];

                for (i, pat) in pats.iter().enumerate() {
                    if !pat.irrefutable {
                        self.emit(Asm::DestructTuple { index: i }, span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(pat, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::List(elements, rest) => {
                let elem_count = elements.len();
                if rest.is_some() {
                    self.emit(Asm::TestListMin { len: elem_count }, span)?;
                } else {
                    self.emit(Asm::TestListExact { len: elem_count }, span)?;
                }
                let len_jump = self.jump_if_false(span)?;
                let mut all_jumps = vec![(len_jump, base_depth)];

                for (i, pat) in elements.iter().enumerate() {
                    if !pat.irrefutable {
                        self.emit(Asm::DestructList { index: i }, span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(pat, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }

                if let Some(rest_pat) = rest
                    && !rest_pat.irrefutable
                {
                    self.emit(Asm::DestructListRest { start: elem_count }, span)?;
                    let sub_fails =
                        self.compile_pattern_test_tracked(rest_pat, span, base_depth + 1)?;
                    self.emit(Asm::Pop, span)?;
                    all_jumps.extend(sub_fails);
                }

                Ok(all_jumps)
            }

            PatternKind::Record { name, fields, .. } => {
                let mut all_jumps = Vec::new();

                let ty = self.pattern_record_type(pattern, span)?;
                if name.is_some()
                    && let Some(ty) = &ty
                {
                    let idx = self.add_constant(Value::TypeDescriptor(ty.clone()), span)?;
                    self.emit(Asm::TestRecordTag { ty: idx }, span)?;
                    let tag_jump = self.jump_if_false(span)?;
                    all_jumps.push((tag_jump, base_depth));
                }

                for (field_name, _, sub_pat) in fields {
                    let sub_pattern = match sub_pat {
                        Some(p) => p,
                        None => continue,
                    };
                    if !sub_pattern.irrefutable {
                        let field = Self::field_read(ty.as_deref(), *field_name);
                        self.emit_destruct_field(field, span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(sub_pattern, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            PatternKind::AnonRecord { fields, .. } => {
                let mut all_jumps = Vec::new();
                for (field_name, _, sub_pat) in fields {
                    let sub_pattern = match sub_pat {
                        Some(p) => p,
                        None => continue,
                    };
                    if !sub_pattern.irrefutable {
                        self.emit_destruct_field(FieldRead::Named(*field_name), span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(sub_pattern, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }
                Ok(all_jumps)
            }

            PatternKind::Map(entries) => {
                let mut all_jumps = Vec::new();

                for (key, sub_pat) in entries {
                    let key_idx = self.add_constant(Value::String(key.clone().into()), span)?;
                    self.emit(Asm::TestMapHasKey { key: key_idx }, span)?;
                    let key_jump = self.jump_if_false(span)?;
                    all_jumps.push((key_jump, base_depth));

                    if !sub_pat.irrefutable {
                        let key_idx2 =
                            self.add_constant(Value::String(key.clone().into()), span)?;
                        self.emit(Asm::DestructMapValue { key: key_idx2 }, span)?;
                        let sub_fails =
                            self.compile_pattern_test_tracked(sub_pat, span, base_depth + 1)?;
                        self.emit(Asm::Pop, span)?;
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            // Try each alternative; the first that matches jumps to
            // success. A failed test inside a compound alternative
            // jumps over the `Pop` of the sub-value it was looking at,
            // so before the next alternative is tested every failure
            // goes through a cleanup trampoline that pops what its depth
            // left (`emit_trampolines`): the next alternative sees the
            // value itself on TOS.
            PatternKind::Or(alternatives) => {
                let mut fail_jumps = Vec::new();
                let mut success_jumps = Vec::new();

                for (i, alt) in alternatives.iter().enumerate() {
                    let sub_fails = self.compile_pattern_test_tracked(alt, span, base_depth)?;

                    if i < alternatives.len() - 1 {
                        let success = self.jump(span)?;
                        success_jumps.push(success);

                        self.emit_trampolines(&sub_fails, base_depth, span)?;
                    } else {
                        // Last alternative: its failures are the
                        // or-pattern's, cleaned up the same way.
                        let max_depth = sub_fails
                            .iter()
                            .map(|&(_, d)| d)
                            .max()
                            .unwrap_or(base_depth);

                        if max_depth <= base_depth {
                            fail_jumps = sub_fails;
                        } else {
                            // Skip over trampolines on success path.
                            let success_skip = self.jump(span)?;

                            let deep: Vec<(Label, usize)> = sub_fails
                                .iter()
                                .copied()
                                .filter(|(_, d)| *d > base_depth)
                                .collect();
                            self.emit_trampolines(&deep, base_depth, span)?;
                            let exit_jump = self.jump(span)?;

                            // Patch success_skip to land after trampolines.
                            self.bind(success_skip, span)?;

                            fail_jumps = sub_fails
                                .into_iter()
                                .filter(|(_, d)| *d <= base_depth)
                                .collect();
                            fail_jumps.push((exit_jump, base_depth));
                        }
                    }
                }

                for sj in success_jumps {
                    self.bind(sj, span)?;
                }

                Ok(fail_jumps)
            }
        }
    }

    /// Emit the cleanup trampolines of the failure jumps `fails` of a
    /// pattern test, each with the number of sub-values its failed test
    /// leaves above the value under test, of which `base_depth` were
    /// there before the test: one `Pop` per depth above `base_depth`,
    /// from the deepest down. A jump lands on the `Pop` of its depth
    /// and runs the ones after it too, so every failure arrives after
    /// the trampolines with only the `base_depth` sub-values left. A
    /// jump no deeper than `base_depth` lands after them.
    fn emit_trampolines(
        &mut self,
        fails: &[(Label, usize)],
        base_depth: usize,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let max_depth = fails.iter().map(|&(_, d)| d).max().unwrap_or(base_depth);
        for depth in (base_depth + 1..=max_depth).rev() {
            for (jump, _) in fails.iter().filter(|(_, d)| *d == depth) {
                self.bind(*jump, span)?;
            }
            self.emit(Asm::Pop, span)?;
        }
        for (jump, _) in fails.iter().filter(|(_, d)| *d <= base_depth) {
            self.bind(*jump, span)?;
        }
        Ok(())
    }

    // ── Pattern bind in a position without an alternative ───────

    /// Bind `pattern` to the value on TOS where a failed match has
    /// nowhere to go: `let`, and the parameters of functions, closures
    /// and trait methods. The typechecker only lets patterns through
    /// that match every value of their type. Should another pattern
    /// arrive here, the program stops instead of running with names
    /// bound to the parts of a value of a different shape: either the
    /// destructuring itself fails (a list that is too short, a variant
    /// without the field), or, if it went through, the outcome of the
    /// pattern's test does.
    ///
    /// Same contract as `compile_pattern_bind`.
    pub(super) fn compile_pattern_bind_checked(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if pattern.irrefutable {
            return self.compile_pattern_bind(pattern, span);
        }
        // The value is the local on top of the frame.
        let value_slot = self.emitter().height().saturating_sub(1);

        // Test the pattern and keep the outcome as a hidden local.
        let fail_jumps = self.compile_pattern_test(pattern, span)?;
        self.emit(Asm::True, span)?;
        let tested = self.jump(span)?;
        for fail_jump in fail_jumps {
            self.bind(fail_jump, span)?;
        }
        // A failed test of a nested pattern leaves the sub-values it was
        // looking at above the value; drop them.
        self.emit(Asm::GetLocal { slot: value_slot }, span)?;
        self.emit(Asm::Slide { slot: value_slot }, span)?;
        self.emit(Asm::False, span)?;
        self.bind(tested, span)?;
        let matched_slot = self.add_local(intern("__bind_matched__"), span)?;

        // Bind from a copy of the value, which is on TOS again.
        self.emit(Asm::GetLocal { slot: value_slot }, span)?;
        self.compile_pattern_bind(pattern, span)?;

        // The destructuring went through. Stop if the test had failed.
        self.emit(Asm::GetLocal { slot: matched_slot }, span)?;
        let matched = self.jump_if_true(span)?;
        let message = self.add_constant(
            Value::String("the value does not match the pattern it is bound to".into()),
            span,
        )?;
        self.emit(Asm::Constant { k: message }, span)?;
        self.emit(Asm::Panic, span)?;
        self.bind(matched, span)?;
        Ok(())
    }

    // ── Recursive pattern bind ───────────────────────────────────
    //
    // Emit binding opcodes for a pattern after test has succeeded.
    //
    // Contract: the value to bind from is on TOS. After this call the
    // value is still in its slot, and every value pushed here stays in
    // the frame above it: the named locals the pattern binds, and the
    // copies and sub-values the destructuring went through.
    //
    // Stack layout for a compound pattern like (a, b):
    //   Before: [..., tuple]
    //   After:  [..., tuple, tuple_copy, tuple_copy, elem0, a,
    //                                    tuple_copy, elem1, b]

    pub(super) fn compile_pattern_bind(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<(), Diagnostic> {
        // The locals there are now are the ones a pin of this pattern
        // can mean; the pattern's own names come after them.
        let outermost = self.ctx().pattern_floor.is_none();
        if outermost {
            let floor = self.ctx().locals.len();
            self.ctx_mut().pattern_floor = Some(floor);
        }
        let result = self.compile_pattern_bind_parts(pattern, span);
        if outermost {
            self.ctx_mut().pattern_floor = None;
        }
        result
    }

    /// The local `name` a pin means: the innermost one that is not a
    /// name of the pattern being bound.
    fn resolve_local_outside_pattern(&self, name: Symbol) -> Option<usize> {
        let ctx = self.ctx();
        let outside = ctx.pattern_floor.unwrap_or(ctx.locals.len());
        ctx.locals[..outside]
            .iter()
            .rev()
            .find(|local| local.name == name)
            .map(|local| local.slot)
    }

    fn compile_pattern_bind_parts(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match &pattern.kind {
            PatternKind::Ident(name) => {
                // Dup the value, the dup'd copy becomes the local's stack slot.
                self.emit(Asm::Dup, span)?;
                let slot = self.add_local(*name, span)?;
                self.emit(Asm::SetLocal { slot }, span)?;
            }

            PatternKind::Constructor { args: fields, .. } => {
                self.compile_compound_bind(
                    fields
                        .iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Variant(i), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            PatternKind::Tuple(pats) => {
                self.compile_compound_bind(
                    pats.iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Tuple(i), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            PatternKind::List(elements, rest) => {
                let mut items: Vec<(BindDestructKind, Pattern)> = elements
                    .iter()
                    .enumerate()
                    .filter_map(|(i, pat)| {
                        if self.pattern_has_bindings(pat) {
                            Some((BindDestructKind::List(i), pat.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                if let Some(rest_pat) = rest
                    && self.pattern_has_bindings(rest_pat)
                {
                    items.push((
                        BindDestructKind::ListRest(elements.len()),
                        (**rest_pat).clone(),
                    ));
                }
                self.compile_compound_bind(items, span)?;
            }

            PatternKind::Record { fields, .. } => {
                let ty = self.pattern_record_type(pattern, span)?;
                let mut items: Vec<(BindDestructKind, Pattern)> = Vec::new();
                for (field_name, _, sub_pat) in fields {
                    let field = Self::field_read(ty.as_deref(), *field_name);
                    match sub_pat {
                        Some(pat) => {
                            if self.pattern_has_bindings(pat) {
                                items.push((BindDestructKind::RecordField(field), pat.clone()));
                            }
                        }
                        None => {
                            // Shorthand: { name } binds field to local with same name
                            items.push((
                                BindDestructKind::RecordField(field),
                                Pattern::new(PatternKind::Ident(*field_name), pattern.span),
                            ));
                        }
                    }
                }
                self.compile_compound_bind(items, span)?;
            }

            PatternKind::AnonRecord { fields, rest } => {
                let mut items: Vec<(BindDestructKind, Pattern)> = Vec::new();
                for (field_name, _, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => {
                            if self.pattern_has_bindings(pat) {
                                items.push((
                                    BindDestructKind::RecordField(FieldRead::Named(*field_name)),
                                    pat.clone(),
                                ));
                            }
                        }
                        None => {
                            items.push((
                                BindDestructKind::RecordField(FieldRead::Named(*field_name)),
                                Pattern::new(PatternKind::Ident(*field_name), pattern.span),
                            ));
                        }
                    }
                }
                if let Some((rest_name, _)) = rest {
                    // The rest-capture must run against the parent record,
                    // not against any per-field sub-value. Funnel it through
                    // `compile_compound_bind` so it shares the
                    // `__bind_parent__` slot used by every other field
                    // destructure — that way the rest opcode is fed the
                    // actual parent on every iteration regardless of which
                    // sub-value happens to be on TOS.
                    let names: Vec<Symbol> = fields.iter().map(|(n, _, _)| *n).collect();
                    items.push((
                        BindDestructKind::RecordRest(names),
                        Pattern::new(PatternKind::Ident(*rest_name), pattern.span),
                    ));
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
                let Some(first) = alternatives.first() else {
                    return Ok(());
                };
                let expected = Self::pattern_binding_names(first);
                for alt in &alternatives[1..] {
                    let actual = Self::pattern_binding_names(alt);
                    if actual != expected {
                        return Err(Diagnostic::error(
                            Code::InvalidConstruct,
                            span,
                            "or-pattern alternatives must bind the same variables",
                        ));
                    }
                }

                if expected.is_empty() {
                    // No bindings in any alternative — nothing to extract.
                    return Ok(());
                }

                if alternatives.len() == 1 {
                    self.compile_pattern_bind(first, span)?;
                    return Ok(());
                }

                // Multiple alternatives that bind variables. The shared
                // variables may sit at *different* structural positions in
                // each alternative (e.g. `A(x) | B(_, x)`), so we cannot
                // bind from a single alternative. Instead, re-test each
                // alternative against the scrutinee and run *that*
                // alternative's own bind sequence, funnelling every
                // alternative's results into one shared set of result slots.
                //
                // The overall pattern test already succeeded before this
                // runs, so at least one alternative is guaranteed to match;
                // the last alternative therefore needs no re-test (it is the
                // fallthrough).
                //
                // TOS = scrutinee on entry. Save it to a hidden local so each
                // alternative can fetch a fresh copy for its test+bind.
                self.emit(Asm::Dup, span)?;
                let scrut_slot = self.add_local(intern("__or_bind_scrut__"), span)?;
                self.emit(Asm::SetLocal { slot: scrut_slot }, span)?;

                // Reserve one result slot per bound name (deterministic
                // order from the BTreeSet). Each alternative writes the value
                // for each name into its fixed result slot, so the body
                // resolves every name to the same slot regardless of which
                // alternative matched.
                let names: Vec<Symbol> = expected.iter().copied().collect();
                let mut result_slots = Vec::with_capacity(names.len());
                for name in &names {
                    self.emit(Asm::Unit, span)?;
                    let slot = self.add_local(*name, span)?;
                    result_slots.push(slot);
                }

                // Baseline: everything pushed past this point by an
                // alternative's test/bind is a temporary to be cleaned up.
                let baseline_locals = self.ctx().locals.len();
                let baseline_height = self.emitter().height();

                let mut end_jumps = Vec::new();
                let last = alternatives.len() - 1;
                for (i, alt) in alternatives.iter().enumerate() {
                    // Fetch a fresh scrutinee copy for this alternative.
                    self.emit(Asm::GetLocal { slot: scrut_slot }, span)?;

                    // Non-last alternatives re-test; the last is the
                    // guaranteed-matching fallthrough.
                    let fails = if i < last {
                        self.compile_pattern_test_tracked(alt, span, 0)?
                    } else {
                        Vec::new()
                    };

                    // Matched: bind this alternative into temporary locals,
                    // then copy each bound value into its shared result slot.
                    self.compile_pattern_bind(alt, span)?;
                    for (ni, name) in names.iter().enumerate() {
                        let temp = self.resolve_local(*name).expect(
                            "or-pattern alternative must bind every shared name (validated above)",
                        );
                        self.emit(Asm::GetLocal { slot: temp }, span)?;
                        self.emit(
                            Asm::SetLocal {
                                slot: result_slots[ni],
                            },
                            span,
                        )?;
                        self.emit(Asm::Pop, span)?;
                    }

                    // Pop every temporary this alternative pushed (the
                    // scrutinee copy plus all bind intermediates), restoring
                    // the stack to the baseline: the temp count is the
                    // growth of the frame's height.
                    let temps = self.emitter().height().saturating_sub(baseline_height);
                    for _ in 0..temps {
                        self.emit(Asm::Pop, span)?;
                    }
                    // Unregister the temporaries so the next alternative
                    // reuses the same slots and the final local state holds
                    // only the result slots.
                    self.ctx_mut().locals.truncate(baseline_locals);

                    if i < last {
                        // Success path: skip the remaining alternatives.
                        let done = self.jump(span)?;
                        end_jumps.push(done);

                        // Failure path: clean up any destructure
                        // intermediates (per-depth trampolines), then the
                        // leftover scrutinee copy, before falling through to
                        // the next alternative.
                        self.emit_trampolines(&fails, 0, span)?;
                        // Depth-0 landing: pop the leftover scrutinee copy
                        // that the test peeked but did not consume. The next
                        // alternative's GetLocal follows immediately.
                        self.emit(Asm::Pop, span)?;
                    }
                }

                for j in end_jumps {
                    self.bind(j, span)?;
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
    /// we GetLocal the parent, Destruct the sub-value, leave both in the
    /// frame, and recurse.
    ///
    /// This approach "wastes" stack slots for intermediate copies but ensures
    /// local slot numbers always match actual stack positions.
    fn compile_compound_bind(
        &mut self,
        items: Vec<(BindDestructKind, Pattern)>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if items.is_empty() {
            return Ok(());
        }

        // The parent is on TOS. A copy of it becomes a hidden local, read
        // once for every sub-pattern.
        self.emit(Asm::Dup, span)?;
        let parent_slot = self.add_local(intern("__bind_parent__"), span)?;
        self.emit(Asm::SetLocal { slot: parent_slot }, span)?;

        for (kind, sub_pat) in &items {
            // Push the parent value from the known slot
            self.emit(Asm::GetLocal { slot: parent_slot }, span)?;

            // Destruct to get the sub-value
            match kind {
                BindDestructKind::Variant(i) => {
                    self.emit(Asm::DestructVariant { index: *i }, span)?;
                }
                BindDestructKind::Tuple(i) => {
                    self.emit(Asm::DestructTuple { index: *i }, span)?;
                }
                BindDestructKind::List(i) => {
                    self.emit(Asm::DestructList { index: *i }, span)?;
                }
                BindDestructKind::ListRest(start) => {
                    self.emit(Asm::DestructListRest { start: *start }, span)?;
                }
                BindDestructKind::RecordField(field) => {
                    self.emit_destruct_field(*field, span)?;
                }
                BindDestructKind::RecordRest(names) => {
                    // `Op::DestructRecordRest` pops its input and pushes the
                    // remainder record. The surrounding loop expects each
                    // destruct opcode to peek+push so that
                    // `[parent_copy, sub_value]` is on the stack afterwards.
                    // Dup the parent_copy first to bridge the contract gap.
                    self.emit(Asm::Dup, span)?;
                    let excluded = self.name_constants(names, span)?;
                    self.emit(
                        Asm::DestructRecordRest {
                            excluded: &excluded,
                        },
                        span,
                    )?;
                }
                BindDestructKind::MapValue(key) => {
                    let key_idx = self.add_constant(Value::String(key.clone().into()), span)?;
                    self.emit(Asm::DestructMapValue { key: key_idx }, span)?;
                }
            }

            // Stack: [..., parent_copy_from_GetLocal, sub_value]
            // Both stay in the frame; the sub-value is on TOS, as the
            // nested pattern's bind expects.

            // Recurse into the sub-pattern for binding
            self.compile_pattern_bind(sub_pat, span)?;
        }

        Ok(())
    }

    // ── Pattern analysis helpers ─────────────────────────────────

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
            PatternKind::Constructor { args: fields, .. } => {
                fields.iter().any(|p| self.pattern_has_bindings(p))
            }
            PatternKind::Tuple(pats) => pats.iter().any(|p| self.pattern_has_bindings(p)),
            PatternKind::List(elems, rest) => {
                elems.iter().any(|p| self.pattern_has_bindings(p))
                    || rest.as_ref().is_some_and(|r| self.pattern_has_bindings(r))
            }
            PatternKind::Record { fields, .. } => fields.iter().any(|(_, _, p)| {
                match p {
                    Some(pat) => self.pattern_has_bindings(pat),
                    None => true, // shorthand {name} always binds
                }
            }),
            PatternKind::AnonRecord { fields, rest } => {
                rest.is_some()
                    || fields.iter().any(|(_, _, p)| match p {
                        Some(pat) => self.pattern_has_bindings(pat),
                        None => true,
                    })
            }
            PatternKind::Or(alts) => alts.iter().any(|p| self.pattern_has_bindings(p)),
            PatternKind::Map(entries) => entries.iter().any(|(_, p)| self.pattern_has_bindings(p)),
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
            PatternKind::Constructor { args: fields, .. } => {
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
                for (field_name, _, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => Self::collect_binding_names(pat, names),
                        None => {
                            names.insert(*field_name);
                        }
                    }
                }
            }
            PatternKind::AnonRecord { fields, rest } => {
                for (field_name, _, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => Self::collect_binding_names(pat, names),
                        None => {
                            names.insert(*field_name);
                        }
                    }
                }
                if let Some((r, _)) = rest {
                    names.insert(*r);
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

#[cfg(test)]
mod tests {
    use crate::bytecode::{Function, Op};
    use crate::value::Value;

    /// The function `name` of the compiled declarations `input`.
    fn compiled(input: &str, name: &str) -> Function {
        fn find(functions: &[&Function], name: &str) -> Option<Function> {
            for f in functions {
                if f.name() == name {
                    return Some((*f).clone());
                }
                let nested: Vec<&Function> = f
                    .chunk()
                    .constants()
                    .iter()
                    .filter_map(|c| match c {
                        Value::VmClosure(closure) => Some(&*closure.function),
                        _ => None,
                    })
                    .collect();
                if let Some(found) = find(&nested, name) {
                    return Some(found);
                }
            }
            None
        }
        let program =
            crate::session::testing::compile_decls_str(input).unwrap_or_else(|e| panic!("{e:?}"));
        find(&program.functions.iter().collect::<Vec<_>>(), name)
            .unwrap_or_else(|| panic!("no function '{name}'"))
    }

    fn has_op(function: &Function, op: Op) -> bool {
        function.chunk().instrs().any(|(_, instr)| instr.op() == op)
    }

    const TYPES: &str = "type Wrap { Wrap(Int) }\ntype Point { x: Int, y: Int }\n";

    /// A `let` or a parameter whose pattern the checker marked
    /// irrefutable is bound without a test.
    #[test]
    fn test_irrefutable_binding_has_no_test() {
        let f = compiled(
            "fn f((a, {x, y}), ()) -> Int {\n  let (c, (d, _)) = (x, (y, a))\n  a + c + d\n}\n",
            "f",
        );
        for op in [Op::TestTupleLen, Op::TestEqual, Op::Panic] {
            assert!(!has_op(&f, op), "{op:?} emitted for an irrefutable binding");
        }
        assert!(has_op(&f, Op::DestructTuple));
        assert!(has_op(&f, Op::DestructRecordField));
    }

    /// The one variant of an enum and a record type are irrefutable too:
    /// no test of their tag, in a binding (which cannot stop) and in an
    /// arm.
    #[test]
    fn test_nominal_patterns_marked_irrefutable_have_no_tag_test() {
        let f = compiled(
            &format!("{TYPES}fn f(Wrap(a), Point {{ x, y }}) -> Int {{ a + x + y }}\n"),
            "f",
        );
        for op in [Op::TestTag, Op::TestRecordTag, Op::Panic] {
            assert!(!has_op(&f, op), "{op:?} emitted for an irrefutable binding");
        }
        assert!(has_op(&f, Op::DestructVariant));
        assert!(has_op(&f, Op::DestructRecordField));

        let g = compiled(
            &format!(
                "{TYPES}fn g(o: Option((Wrap, Point))) -> Int {{\n  match o {{\n    \
                 Some((Wrap(a), Point {{ x, y }})) -> a + x + y\n    None -> 0\n  }}\n}}\n"
            ),
            "g",
        );
        for op in [Op::TestRecordTag, Op::TestTupleLen] {
            assert!(!has_op(&g, op), "{op:?} emitted inside an arm's pattern");
        }
        // (`Some(..)` can fail, and is tested.)
        assert!(has_op(&g, Op::TestTag));
    }

    /// A pattern that can fail keeps its test, and the test of its own
    /// shape with it: the mark says whether a pattern can fail, not which
    /// of its tests can.
    #[test]
    fn test_refutable_parts_are_tested() {
        let f = compiled(
            &format!(
                "{TYPES}fn f(p: (Wrap, Bool)) -> Int {{\n  match p {{\n    \
                 (Wrap(0), _) -> 0\n    (Wrap(a), true) -> a\n    (_, false) -> 1\n  }}\n}}\n"
            ),
            "f",
        );
        assert!(has_op(&f, Op::TestEqual));
        assert!(has_op(&f, Op::DestructVariant));
        assert!(has_op(&f, Op::TestTag));
    }
}
