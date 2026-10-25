//! The dictionary instructions (`crate::bytecode::dict`): what the VM
//! does to make dictionaries, to take a method out of one, and to call a
//! definition with its dictionaries bound.

use std::sync::Arc;

use crate::bytecode::VmClosure;
use crate::bytecode::dict::Dict;
use crate::value::Value;

use super::{Vm, VmError};

/// The dictionary `value` is, as the verified code that names it holds
/// one there.
fn dict(value: &Value) -> Result<&Dict, VmError> {
    match value {
        Value::Dict(dict) => Ok(dict),
        other => Err(not_a_dict(other)),
    }
}

/// The error of a value that is no dictionary where one is.
#[cold]
fn not_a_dict(value: &Value) -> VmError {
    VmError::type_confusion(format!(
        "a dictionary was expected, not a value of kind {}",
        value.kind()
    ))
}

/// The error of a dictionary the program has no template for.
#[cold]
fn no_template() -> VmError {
    VmError::type_confusion("a dictionary of an impl the program does not have")
}

impl Vm {
    /// The value of the global slot `slot`: a closure as this VM uses
    /// it (its own copy), or the value the program's newest globals
    /// have, for the VM of a task of an earlier REPL entry.
    fn global_value(&mut self, slot: u16) -> Result<Value, VmError> {
        match self.view.global(slot) {
            Some(Value::VmClosure(shared)) => Ok(Value::VmClosure(self.own.of(slot, shared))),
            Some(value) => Ok(value.clone()),
            None => {
                let newest = &self.runtime.newest;
                let value = (self.view).global_of_newest(&mut self.view_count, newest, slot)?;
                Ok(self.own.value(slot, value))
            }
        }
    }

    /// The template of the dictionary `dict` is not in this VM's globals:
    /// it is of a later REPL entry than the VM's view, which a task of an
    /// earlier one meets (in code a later one sent it). The VM takes the
    /// program's newest view, which has it.
    #[cold]
    #[inline(never)]
    fn see_template(&mut self, index: u32) -> Result<(), VmError> {
        self.look_again();
        match self.view.entry.template_at(index) {
            Some(_) => Ok(()),
            None => Err(no_template()),
        }
    }

    /// [`Vm::see_template`] of the dictionary in the stack's slot `at`
    /// if this VM's globals do not have its template.
    fn sees_template_at(&mut self, at: usize) -> Result<(), VmError> {
        let dict = dict(&self.stack[at])?;
        if dict.template(&self.view.entry).is_some() {
            return Ok(());
        }
        let index = dict.index();
        self.see_template(index)
    }

    /// `MakeDict`: the dictionary of the impl whose dictionary `proto`
    /// is, of the top `n` values.
    pub(super) fn make_dict(&mut self, proto: &Value, n: usize) -> Result<(), VmError> {
        let proto = dict(proto)?;
        if proto.template(&self.view.entry).is_none() {
            self.see_template(proto.index())?;
        }
        let template = (proto.template(&self.view.entry).cloned()).ok_or_else(no_template)?;
        let dict = Dict::new(&template, self.stack.drain(self.stack.len() - n..));
        self.stack.push(Value::Dict(dict));
        Ok(())
    }

    /// `CallDict`: the method `method` of the dictionary below the top
    /// `argc` values, which the call calls with them. The argument `at`
    /// of them (none: `u8::MAX`) is the receiver, which is of the
    /// dictionary's type unless the checker missed something.
    #[inline]
    pub(super) fn dict_callee(
        &mut self,
        method: usize,
        argc: usize,
        at: usize,
    ) -> Result<Value, VmError> {
        let first = self.stack.len() - argc;
        let dict = dict(&self.stack[first - 1])?;
        let Some(template) = dict.template(&self.view.entry) else {
            let index = dict.index();
            self.see_template(index)?;
            return self.dict_callee(method, argc, at);
        };
        let (Some(&slot), ty) = (template.methods.get(method), template.ty) else {
            return Err(VmError::type_confusion(format!(
                "the dictionary of {} has no method {method}",
                template.name
            )));
        };
        if at != usize::from(u8::MAX)
            && crate::types::canonical::dispatch_type_for_value(&self.stack[first + at]) != ty
        {
            return Err(self.wrong_receiver(first - 1, at + 1));
        }
        match self.view.global(slot) {
            Some(Value::VmClosure(shared)) => Ok(Value::VmClosure(self.own.of(slot, shared))),
            _ => self.global_value(slot),
        }
    }

    /// The error of a call through the dictionary in the stack's slot
    /// `first` whose receiver, `at` after it, is of another type.
    #[cold]
    #[inline(never)]
    fn wrong_receiver(&self, first: usize, at: usize) -> VmError {
        let name = match &self.stack[first] {
            Value::Dict(dict) => dict.template(&self.view.entry).map(|t| t.name.clone()),
            _ => None,
        };
        VmError::type_confusion(format!(
            "a call through the dictionary of {} has a receiver of type '{}'",
            name.unwrap_or_default(),
            crate::types::canonical::dispatch_type_name(&self.stack[first + at])
        ))
    }

    /// `CallChecked`: the receiver in the stack's slot `at` is of the
    /// type of the impl whose template is the program's `template`th, as
    /// the checker said, unless the checker missed something.
    #[inline]
    pub(super) fn check_receiver(&mut self, template: usize, at: usize) -> Result<(), VmError> {
        let index = u32::try_from(template).unwrap_or(u32::MAX);
        let Some(found) = self.view.entry.template_at(index) else {
            self.see_template(index)?;
            return self.check_receiver(template, at);
        };
        if crate::types::canonical::dispatch_type_for_value(&self.stack[at]) == found.ty {
            return Ok(());
        }
        Err(self.wrong_impl_receiver(index, at))
    }

    /// The error of a call of a method of the impl of the `index`th
    /// template whose receiver, in the stack's slot `at`, is of another
    /// type.
    #[cold]
    #[inline(never)]
    fn wrong_impl_receiver(&self, index: u32, at: usize) -> VmError {
        let name = self.view.entry.template_at(index).map(|t| t.name.clone());
        VmError::type_confusion(format!(
            "a call of a method of {} has a receiver of type '{}'",
            name.unwrap_or_default(),
            crate::types::canonical::dispatch_type_name(&self.stack[at])
        ))
    }

    /// `DictMethod`: the method `method` of the dictionary on top, then
    /// the dictionary.
    ///
    /// Here, in `DictSub`, `SuperDict`, `WhereDict` and `Bind`, unit is a
    /// dictionary no code will use: the one a part of a pattern's value
    /// needs that another name of the pattern takes
    /// (`Compiler::compile_pattern_thunk`). What is made of it is unit.
    pub(super) fn dict_method(&mut self, method: usize, of: u16) -> Result<(), VmError> {
        let top = self.stack.len() - 1;
        if matches!(self.stack[top], Value::Unit) {
            self.stack[top] = self.undecided_method(method, of);
            self.stack.push(Value::Unit);
            return Ok(());
        }
        self.sees_template_at(top)?;
        let template = dict(&self.stack[top])?.template(&self.view.entry);
        let slot = template.and_then(|template| template.methods.get(method).copied());
        let Some(slot) = slot else {
            return Err(no_template());
        };
        let callee = self.global_value(slot)?;
        let dict = std::mem::replace(&mut self.stack[top], callee);
        self.stack.push(dict);
        Ok(())
    }

    /// `DictMethod` of no dictionary: the method `method` of the trait
    /// `of` (the trait's `trait_index`) where no type was decided for it,
    /// a value that is shown as the method is (`<fn:Nm.nm>`) and is
    /// never called: the value of a REPL entry general in a bound, which
    /// the entry only shows (`Compiler::compile_program`).
    #[cold]
    fn undecided_method(&self, method: usize, of: u16) -> Value {
        let globals = &self.view.entry;
        let trait_name = globals.trait_name(of).unwrap_or("?");
        let method_name = (globals.trait_at(of))
            .and_then(|t| globals.trait_methods(t))
            .and_then(|methods| methods.get(method).map(|(name, _)| name.clone()))
            .unwrap_or_default();
        let name = format!("{trait_name}.{method_name}");
        let message = format!("the method '{name}' was called with no type decided for it");
        let code = vec![
            crate::bytecode::Op::Constant as u8,
            0,
            0,
            crate::bytecode::Op::Panic as u8,
        ];
        let function =
            crate::bytecode::Function::of_code(name, code, vec![Value::String(message.into())]);
        Value::VmClosure(Arc::new(VmClosure::new(Arc::new(function), vec![])))
    }

    /// `DictSub`, `SuperDict`, `WhereDict`: the dictionary on top's
    /// dictionary that `which` takes from it.
    pub(super) fn dict_part(
        &mut self,
        which: impl FnOnce(&Dict, &crate::bytecode::Globals) -> Value,
    ) -> Result<(), VmError> {
        let top = self.stack.len() - 1;
        if matches!(self.stack[top], Value::Unit) {
            return Ok(());
        }
        self.sees_template_at(top)?;
        let part = which(dict(&self.stack[top])?, &self.view.entry);
        self.stack[top] = part;
        Ok(())
    }

    /// `Bind`: the closure below the top `n` values, which, called, is
    /// called with those values before its arguments (with the first in
    /// the slot of the callee, for a method that takes its dictionary
    /// from there).
    pub(super) fn bind(&mut self, n: usize) -> Result<(), VmError> {
        let values: Vec<Value> = self.stack.drain(self.stack.len() - n..).collect();
        let top = self.stack.len() - 1;
        if matches!(self.stack[top], Value::Unit) {
            return Ok(());
        }
        let Value::VmClosure(closure) = &self.stack[top] else {
            return Err(VmError::type_confusion(format!(
                "dictionaries bound to a value of kind {}",
                self.stack[top].kind()
            )));
        };
        let mut bound = VmClosure::clone(closure);
        bound.bound = closure.bound.iter().cloned().chain(values).collect();
        self.stack[top] = Value::VmClosure(Arc::new(bound));
        Ok(())
    }
}

/// The arguments of a call of `closure` from the stack's slot `at` on,
/// `argc` of them: with the values a `Bind` gave the closure put before
/// them, the first in the slot below them (the callee's) if the closure
/// takes its dictionary from there. Their number now.
pub(super) fn with_bound(
    stack: &mut Vec<Value>,
    closure: &VmClosure,
    at: usize,
    argc: usize,
) -> usize {
    let Some((first, rest)) = closure.bound.split_first() else {
        return argc;
    };
    if closure.function.dict_slot() {
        stack[at - 1] = first.clone();
        stack.splice(at..at, rest.iter().cloned());
        return argc + rest.len();
    }
    stack.splice(at..at, closure.bound.iter().cloned());
    argc + closure.bound.len()
}
