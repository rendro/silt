//! Pattern matching compilation for Silt.
//!
//! This module contains the pattern test, bind, and analysis methods
//! used by the compiler to emit bytecode for pattern matching constructs
//! (match arms, let-destructuring, function parameters, etc.).

use crate::ast::{MatchArm, Pattern, PatternKind};
use crate::bytecode::{Asm, Label};
use crate::intern::{Symbol, intern};
use crate::source::Span;
use crate::value::Value;

use super::{
    BindDestructKind, Compiler, FieldRead, MATCH_ARM, checker_missed, name_without_binding,
};
use crate::diagnostic::Diagnostic;

/// The most ways an or-pattern under a guard is taken apart into (see
/// [`Compiler::guarded_or_ways`]): each is compiled.
const MAX_GUARDED_OR_WAYS: usize = 1024;

impl Compiler<'_> {
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

    // ── A match on literals ──────────────────────────────────────

    /// Compile the arms of a `match` whose scrutinee, the value on top
    /// of the frame, is only held against literals, if the arms have one
    /// of the two shapes below; `false`, and nothing emitted, if they
    /// have not. No arm may have a guard.
    ///
    /// - Two arms, the first `true` or `false`, the second the other of
    ///   the two, `_` or a name: one conditional jump takes the
    ///   scrutinee off the frame and picks the arm.
    /// - Arms that are Int, Float or String literals, and a last one
    ///   that is `_` or a name: each literal is one test of the
    ///   scrutinee where it is, the test any pattern of that literal
    ///   makes ([`Compiler::compile_pattern_test`]).
    ///
    /// The scrutinee is no local here, and no copy of it is made for a
    /// test or for an arm: an arm starts with the frame as it was
    /// before the scrutinee, and leaves its value where the scrutinee
    /// was. An arm that is a name has the value under that name: the
    /// scrutinee itself, or for a Bool the one value that reaches the
    /// arm.
    ///
    /// Every arm of these shapes can be reached and every value of the
    /// scrutinee's type has one, so each arm is compiled, once, and the
    /// code has no end where no arm matched.
    pub(super) fn compile_literal_match(
        &mut self,
        arms: &[MatchArm],
        span: Span,
        tail: bool,
    ) -> Result<bool, Diagnostic> {
        let [tests @ .., last] = arms else {
            return Ok(false);
        };
        if tests.is_empty() || arms.iter().any(|arm| arm.guard.is_some()) {
            return Ok(false);
        }
        let last_name = match &last.pattern.kind {
            PatternKind::Wildcard => None,
            PatternKind::Ident(name) => Some(*name),
            // (The other literal of a Bool: looked at below.)
            PatternKind::Bool(_) => None,
            _ => return Ok(false),
        };
        let mut end_jumps = Vec::new();
        match (&tests[0].pattern.kind, &last.pattern.kind) {
            (PatternKind::Bool(first), other) if tests.len() == 1 => {
                if matches!(other, PatternKind::Bool(second) if second == first) {
                    return Ok(false);
                }
                let otherwise = match first {
                    true => self.jump_if_false(span)?,
                    false => self.jump_if_true(span)?,
                };
                self.in_tail_position = tail;
                self.compile_expr(&tests[0].body)?;
                end_jumps.push(self.jump(span)?);
                self.bind_over(otherwise, MATCH_ARM, span)?;
                match last_name {
                    Some(name) => {
                        self.begin_scope();
                        self.emit(if *first { Asm::False } else { Asm::True }, span)?;
                        self.add_local(name, span)?;
                        self.in_tail_position = tail;
                        self.compile_expr(&last.body)?;
                        self.end_scope_with_result(tail, span)?;
                    }
                    None => {
                        self.in_tail_position = tail;
                        self.compile_expr(&last.body)?;
                    }
                }
            }
            (_, PatternKind::Wildcard | PatternKind::Ident(_))
                if tests.iter().all(|arm| {
                    matches!(
                        arm.pattern.kind,
                        PatternKind::Int(_) | PatternKind::Float(_) | PatternKind::StringLit(..)
                    )
                }) =>
            {
                for arm in tests {
                    // (The test looks at the scrutinee and leaves it.)
                    let fail_jumps = self.compile_pattern_test(&arm.pattern, span)?;
                    self.emit(Asm::Pop, span)?;
                    self.in_tail_position = tail;
                    self.compile_expr(&arm.body)?;
                    end_jumps.push(self.jump(span)?);
                    for fail_jump in fail_jumps {
                        self.bind_over(fail_jump, MATCH_ARM, span)?;
                    }
                }
                match last_name {
                    Some(name) => {
                        self.begin_scope_with_top();
                        self.add_local(name, span)?;
                        self.in_tail_position = tail;
                        self.compile_expr(&last.body)?;
                        self.end_scope_with_result(tail, span)?;
                    }
                    None => {
                        self.emit(Asm::Pop, span)?;
                        self.in_tail_position = tail;
                        self.compile_expr(&last.body)?;
                    }
                }
            }
            _ => return Ok(false),
        }
        for end_jump in end_jumps {
            self.bind(end_jump, span)?;
        }
        Ok(true)
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
                let idx = self.float_constant(*n, span)?;
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
                let lo_idx = self.float_constant(*lo, span)?;
                let hi_idx = self.float_constant(*hi, span)?;
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
                let differs = |alt: &&Pattern| Self::pattern_binding_names(alt) != expected;
                if let Some(alt) = alternatives[1..].iter().find(differs) {
                    let what = "an or-pattern whose alternatives bind different names";
                    return Err(checker_missed(alt.span, what));
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

    // ── A guard on an or-pattern ─────────────────────────────────

    /// The ways the pattern of `arm` can match, when the arm has a guard
    /// and an or-pattern in it binds names: one pattern for each choice
    /// of alternatives, in order (`(x, _) | (_, x)` is `(x, _)`, then
    /// `(_, x)`). The guard is tried with each way that matches, so it
    /// sees each of the values the names can stand for. `None` when
    /// there is one way.
    pub(super) fn guarded_or_ways(
        &self,
        arm: &MatchArm,
    ) -> Result<Option<Vec<Pattern>>, Diagnostic> {
        if arm.guard.is_none() {
            return Ok(None);
        }
        let count = Self::or_way_count(&arm.pattern);
        if count <= 1 {
            return Ok(None);
        }
        if count > MAX_GUARDED_OR_WAYS {
            let limit = crate::bytecode::ops::Limit {
                what: "alternatives of an or-pattern under a guard",
                count,
                max: MAX_GUARDED_OR_WAYS,
            };
            return Err(crate::bytecode::emit::limit_diagnostic(
                limit,
                arm.pattern.span,
            ));
        }
        Ok(Some(Self::or_ways(&arm.pattern)))
    }

    /// How many ways [`Compiler::or_ways`] gives for `pattern`.
    fn or_way_count(pattern: &Pattern) -> usize {
        let product = |parts: &mut dyn Iterator<Item = &Pattern>| {
            parts.fold(1usize, |ways, part| {
                ways.saturating_mul(Self::or_way_count(part))
            })
        };
        match &pattern.kind {
            PatternKind::Or(alternatives) if !Self::pattern_binding_names(pattern).is_empty() => {
                (alternatives.iter()).fold(0usize, |ways, alt| {
                    ways.saturating_add(Self::or_way_count(alt))
                })
            }
            PatternKind::Constructor { args, .. } => product(&mut args.iter()),
            PatternKind::Tuple(parts) => product(&mut parts.iter()),
            PatternKind::List(elems, _) => product(&mut elems.iter()),
            PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
                product(&mut fields.iter().filter_map(|(_, _, part)| part.as_ref()))
            }
            PatternKind::Map(entries) => product(&mut entries.iter().map(|(_, part)| part)),
            _ => 1,
        }
    }

    /// The ways `pattern` can match: its or-patterns that bind names
    /// taken apart, each alternative in its place, in order. A pattern
    /// that is taken apart is tested in full (it is irrefutable only as
    /// a whole).
    fn or_ways(pattern: &Pattern) -> Vec<Pattern> {
        if Self::or_way_count(pattern) <= 1 {
            return vec![pattern.clone()];
        }
        // Every choice of a way for each part, the first part's slowest.
        fn choices<T: Clone>(parts: Vec<Vec<T>>) -> Vec<Vec<T>> {
            parts.into_iter().fold(vec![Vec::new()], |made, ways| {
                made.iter()
                    .flat_map(|chosen| {
                        ways.iter().map(move |way| {
                            let mut next = chosen.clone();
                            next.push(way.clone());
                            next
                        })
                    })
                    .collect()
            })
        }
        let rebuilt = |kind: PatternKind| Pattern {
            kind,
            irrefutable: false,
            ..pattern.clone()
        };
        let ways = |parts: &[Pattern]| choices(parts.iter().map(Self::or_ways).collect());
        // The ways of the fields of a record pattern, a shorthand field
        // standing for itself.
        let field_ways = |fields: &[(Symbol, Span, Option<Pattern>)]| {
            let parts = (fields.iter())
                .map(|(_, _, part)| match part {
                    Some(part) => Self::or_ways(part).into_iter().map(Some).collect(),
                    None => vec![None],
                })
                .collect();
            (choices(parts).into_iter())
                .map(|chosen| {
                    (fields.iter().zip(chosen))
                        .map(|((name, name_span, _), part)| (*name, *name_span, part))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        match &pattern.kind {
            PatternKind::Or(alternatives) => alternatives.iter().flat_map(Self::or_ways).collect(),
            PatternKind::Tuple(parts) => (ways(parts).into_iter())
                .map(|parts| rebuilt(PatternKind::Tuple(parts)))
                .collect(),
            PatternKind::List(elems, rest) => (ways(elems).into_iter())
                .map(|elems| rebuilt(PatternKind::List(elems, rest.clone())))
                .collect(),
            PatternKind::Constructor {
                qualifier,
                name,
                name_span,
                args,
            } => (ways(args).into_iter())
                .map(|args| {
                    rebuilt(PatternKind::Constructor {
                        qualifier: qualifier.clone(),
                        name: *name,
                        name_span: *name_span,
                        args,
                    })
                })
                .collect(),
            PatternKind::Record {
                module,
                name,
                name_span,
                fields,
                has_rest,
            } => (field_ways(fields).into_iter())
                .map(|fields| {
                    rebuilt(PatternKind::Record {
                        module: module.clone(),
                        name: *name,
                        name_span: *name_span,
                        fields,
                        has_rest: *has_rest,
                    })
                })
                .collect(),
            PatternKind::AnonRecord { fields, rest } => (field_ways(fields).into_iter())
                .map(|fields| {
                    rebuilt(PatternKind::AnonRecord {
                        fields,
                        rest: *rest,
                    })
                })
                .collect(),
            PatternKind::Map(entries) => {
                let parts: Vec<Pattern> = entries.iter().map(|(_, part)| part.clone()).collect();
                (ways(&parts).into_iter())
                    .map(|parts| {
                        let entries = (entries.iter().zip(parts))
                            .map(|((key, _), part)| (key.clone(), part))
                            .collect();
                        rebuilt(PatternKind::Map(entries))
                    })
                    .collect()
            }
            _ => vec![pattern.clone()],
        }
    }

    /// Compile the match arm `arm`, whose guard is tried with each of
    /// `ways` that matches, in order (see [`Compiler::guarded_or_ways`]),
    /// for the scrutinee in `scrutinee_slot`; a copy of it is on top.
    /// Each way binds the arm's names into one slot each; the first
    /// whose guard holds runs the body, which is there once. Returns
    /// the jump to the end of the match and the jumps to the next arm.
    pub(super) fn compile_guarded_or_arm(
        &mut self,
        arm: &MatchArm,
        ways: &[Pattern],
        scrutinee_slot: usize,
        tail: bool,
        span: Span,
    ) -> Result<(Label, Vec<Label>), Diagnostic> {
        let guard = arm
            .guard
            .as_deref()
            .ok_or_else(|| checker_missed(span, "an or-pattern split for a guard it has not"))?;
        // Each way tests a copy of its own.
        self.emit(Asm::Pop, span)?;
        self.begin_scope();
        // The names of the arm, in one slot each. (A pin means a name
        // from before them.)
        let floor = self.ctx().locals.len();
        let names: Vec<Symbol> = Self::pattern_binding_names(&arm.pattern)
            .into_iter()
            .collect();
        let mut slots = Vec::with_capacity(names.len());
        for name in &names {
            self.emit(Asm::Unit, span)?;
            slots.push(self.add_local(*name, span)?);
        }
        let baseline_locals = self.ctx().locals.len();
        let baseline_height = self.emitter().height();

        let mut body_jumps = Vec::new();
        for way in ways {
            self.emit(
                Asm::GetLocal {
                    slot: scrutinee_slot,
                },
                span,
            )?;
            self.ctx_mut().pattern_floor = Some(floor);
            let fails = self.compile_pattern_test_tracked(way, span, 0)?;
            self.compile_pattern_bind(way, span)?;
            for (name, slot) in names.iter().zip(&slots) {
                let bound = self.resolve_local(*name).ok_or_else(|| {
                    checker_missed(
                        way.span,
                        "an or-pattern alternative without a name of the others",
                    )
                })?;
                self.emit(Asm::GetLocal { slot: bound }, span)?;
                self.emit(Asm::SetLocal { slot: *slot }, span)?;
                self.emit(Asm::Pop, span)?;
            }
            self.ctx_mut().pattern_floor = None;
            // The copy and what the binding went through go; the names
            // are the arm's slots again.
            let temps = self.emitter().height().saturating_sub(baseline_height);
            for _ in 0..temps {
                self.emit(Asm::Pop, span)?;
            }
            self.ctx_mut().locals.truncate(baseline_locals);

            self.compile_expr(guard)?;
            let guard_failed = self.jump_if_false(span)?;
            body_jumps.push(self.jump(span)?);

            // The way did not match: what its test left goes, its copy
            // too.
            if !fails.is_empty() {
                self.emit_trampolines(&fails, 0, span)?;
                self.emit(Asm::Pop, span)?;
            }
            self.bind_over(guard_failed, MATCH_ARM, span)?;
        }
        // No way matched with its guard: the next arm.
        let next_arm = self.jump(span)?;

        for jump in body_jumps {
            self.bind_over(jump, MATCH_ARM, span)?;
        }
        self.in_tail_position = tail;
        self.compile_expr(&arm.body)?;
        // (The arm's slots stay under its result until the end of the
        // match, as other arms' bindings do.)
        self.end_scope();
        let end = self.jump(span)?;
        Ok((end, vec![next_arm]))
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
    pub(super) fn pattern_binding_names(pattern: &Pattern) -> std::collections::BTreeSet<Symbol> {
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

    /// The opcodes of the function `name` of `input`, in order.
    fn ops(input: &str, name: &str) -> Vec<Op> {
        let function = compiled(input, name);
        (function.chunk().instrs())
            .map(|(_, instr)| instr.op())
            .collect()
    }

    const TYPES: &str = "type Wrap { Wrap(Int) }\ntype Point { x: Int, y: Int }\n";

    /// A `match` on a Bool with an arm for `true` and one for the rest
    /// (or the other way round) is one conditional jump on the
    /// scrutinee: no copy of it, no test against a constant, no slide.
    #[test]
    fn test_a_match_on_a_bool_is_one_jump() {
        use Op::*;
        assert_eq!(
            ops(
                "fn f(b: Bool) -> Int {\n  match b {\n    true -> 1\n    false -> 2\n  }\n}\n",
                "f"
            ),
            [GetLocal, JumpIfFalse, Constant, Jump, Constant, Return]
        );
        // `false` first: the jump is taken when the scrutinee is true.
        // Not in tail position: the value is where the scrutinee was.
        assert_eq!(
            ops(
                "fn f(b: Bool) -> Int {\n  let v = match b {\n    false -> 1\n    _ -> 2\n  }\n  v + 1\n}\n",
                "f"
            ),
            [
                GetLocal, JumpIfTrue, Constant, Jump, Constant, SetLocal, GetLocal, AddConst,
                Return
            ]
        );
        // A name for the rest is the one value that comes there, and is
        // dropped from under the arm's value.
        assert_eq!(
            ops(
                "fn f(b: Bool) -> Bool {\n  let v = match b {\n    true -> false\n    other -> other\n  }\n  v\n}\n",
                "f"
            ),
            [
                GetLocal,
                JumpIfFalse,
                False,
                Jump,
                False,
                GetLocal,
                Slide,
                SetLocal,
                GetLocal,
                Return
            ]
        );
    }

    /// A `match` on Int literals with a last arm for the rest tests the
    /// scrutinee where it is, once per literal.
    #[test]
    fn test_a_match_on_int_literals_tests_the_scrutinee_where_it_is() {
        use Op::*;
        assert_eq!(
            ops(
                "fn f(n: Int) -> Int {\n  match n {\n    0 -> 10\n    1 -> 11\n    _ -> 12\n  }\n}\n",
                "f"
            ),
            [
                GetLocal,
                TestEqual,
                JumpIfFalse,
                Pop,
                Constant,
                Jump,
                TestEqual,
                JumpIfFalse,
                Pop,
                Constant,
                Jump,
                Pop,
                Constant,
                Return
            ]
        );
        // A name for the rest is the scrutinee itself.
        assert_eq!(
            ops(
                "fn f(n: Int) -> Int {\n  let v = match n {\n    0 -> 10\n    m -> m + 1\n  }\n  v + 1\n}\n",
                "f"
            ),
            [
                GetLocal,
                TestEqual,
                JumpIfFalse,
                Pop,
                Constant,
                Jump,
                GetLocal,
                AddConst,
                Slide,
                SetLocal,
                GetLocal,
                AddConst,
                Return
            ]
        );
    }

    /// Float and String literals are tested as Int literals are: the
    /// one test any pattern of the literal makes, on the scrutinee where
    /// it is.
    #[test]
    fn test_a_match_on_float_or_string_literals_tests_the_scrutinee_where_it_is() {
        use Op::*;
        let two_literals_and_the_rest = [
            GetLocal,
            TestEqual,
            JumpIfFalse,
            Pop,
            Constant,
            Jump,
            TestEqual,
            JumpIfFalse,
            Pop,
            Constant,
            Jump,
            Pop,
            Constant,
            Return,
        ];
        assert_eq!(
            ops(
                "fn f(x: Float) -> Int {\n  match x {\n    0.0 -> 10\n    -1.5 -> 11\n    _ -> 12\n  }\n}\n",
                "f"
            ),
            two_literals_and_the_rest
        );
        assert_eq!(
            ops(
                "fn f(s: String) -> Int {\n  match s {\n    \"\" -> 10\n    \"a\" -> 11\n    _ -> 12\n  }\n}\n",
                "f"
            ),
            two_literals_and_the_rest
        );
        // A name for the rest is the scrutinee itself.
        assert_eq!(
            ops(
                "fn f(s: String) -> String {\n  match s {\n    \"a\" -> \"b\"\n    other -> other\n  }\n}\n",
                "f"
            ),
            [
                GetLocal,
                TestEqual,
                JumpIfFalse,
                Pop,
                Constant,
                Jump,
                GetLocal,
                Return
            ]
        );
    }

    /// Every other `match` keeps its scrutinee as a local and ends in
    /// the stop for a value no arm matched: a guard, one arm, a first
    /// arm that takes everything, an or-pattern, a range, literals
    /// without an arm for the rest.
    #[test]
    fn test_other_matches_keep_the_scrutinee() {
        for body in [
            "match b {\n    true when n > 0 -> 1\n    true -> 2\n    false -> 3\n  }",
            "match b {\n    _ -> 1\n  }",
            "match b {\n    other -> match other {\n      _ -> 2\n    }\n  }",
            "match b {\n    true | false -> 1\n  }",
            "match n {\n    0 | 1 -> 1\n    _ -> 2\n  }",
            "match n {\n    0 -> 1\n    1..5 -> 2\n    _ -> 3\n  }",
            "match n {\n    0 -> 1\n    m when m > 3 -> 2\n    _ -> 3\n  }",
            "match (b, n) {\n    (true, 0) -> 1\n    _ -> 2\n  }",
            "match x {\n    0.0 -> 1\n    0.5..1.5 -> 2\n    _ -> 3\n  }",
            "match s {\n    \"a\" | \"b\" -> 1\n    _ -> 2\n  }",
        ] {
            let f = compiled(
                &format!("fn f(b: Bool, n: Int, x: Float, s: String) -> Int {{\n  {body}\n}}\n"),
                "f",
            );
            assert!(has_op(&f, Op::Panic), "no stop in:\n{body}");
        }
    }

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
        // (An anonymous record's fields are read by their names.)
        assert!(has_op(&f, Op::DestructRecordFieldNamed));
        assert!(!has_op(&f, Op::DestructRecordField));
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
        // (A declared record's fields are read at their places.)
        assert!(has_op(&f, Op::DestructRecordField));
        assert!(!has_op(&f, Op::DestructRecordFieldNamed));

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
