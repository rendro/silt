//! In a debug build, the VM follows the checker's trait evidence
//! (`crate::evidence`) beside the values, as dictionaries would be
//! passed, and checks at each call of a method it finds where the code
//! runs (`CallMethod`) that it finds the impl the evidence names. It
//! dispatches as it always does: the evidence only says whether the
//! checker and the runtime agree.
//!
//! A closure carries the evidence its body sees ([`Env`]): one level
//! for each definition with bounds it is in. A load of a definition
//! with bounds (`GetGlobal`, `GetLocal`, `GetUpvalue` with a site in the
//! function's table) gives the closure loaded the level of its own
//! bounds, worked out from the evidence the site names and the levels
//! the running code has; a closure made in a body (`MakeClosure`) has
//! the body's levels; a method the VM finds gets the level its call's
//! evidence gives it. Calls, tail calls, tasks and the builtins that
//! call back into silt pass the closure, and so its evidence, on.
//!
//! What the evidence names at a call is compared with what the VM
//! found: the impl of the call's trait for the type of the receiver, or
//! none (the VM's own structural method), or the row of the builtin
//! registry that answers for a builtin type (an impl the runtime
//! provides). A written impl may leave out a builtin trait's default the
//! VM supplies (`message` of `Error`, which is the value's display): the
//! impl then has no code for the call, and the VM's default answers. A
//! difference is a
//! disagreement: by default an error of the program, which fails its
//! golden case and is a finding of the oracle; with `SILT_EVIDENCE=report`
//! it is written to stderr and the program goes on. Evidence the VM
//! cannot work out (a closure that reached the call some way that bound
//! nothing) is not checked.

use std::sync::{Arc, OnceLock};

use crate::bytecode::{Globals, VmClosure};
use crate::defs::{TraitId, TypeId};
use crate::evidence::{Evidence, Layout, Site};
use crate::source::Span;
use crate::value::Value;

use super::VmError;

/// The evidence a body sees: the level of the definition `owner`, and
/// those of the definitions around it.
#[derive(Debug)]
pub(crate) struct Env {
    owner: Span,
    /// The evidence of each of the definition's bounds; `None` when it
    /// cannot be worked out.
    grounds: Option<Vec<Ground>>,
    parent: Option<Arc<Env>>,
}

/// Evidence as the VM has it: with no bounds of definitions left in it.
#[derive(Debug, Clone)]
pub(crate) enum Ground {
    /// The impl of `tr` for `ty`, with the evidence of its header's
    /// predicates (`None`: not known). (Shared: a call passes evidence
    /// on at no cost of its size, as polymorphic recursion makes it
    /// deeper at each call.)
    Impl {
        tr: TraitId,
        ty: TypeId,
        subs: Option<Arc<[Ground]>>,
    },
    /// The trait `tr` for `ty`, by an impl or by its structure: a
    /// supertrait of a trait of the type.
    Super { tr: TraitId, ty: TypeId },
    /// The trait `tr` by the type's structure.
    Structural(TraitId),
    /// The impl of `tr` for the builtin type `ty` the runtime provides.
    Native { tr: TraitId, ty: TypeId },
    /// The checker had no evidence.
    Missing,
    /// The VM cannot work it out here.
    Unknown,
}

/// Where disagreements are written, and let go: `SILT_EVIDENCE=report`
/// writes them to stderr, `SILT_EVIDENCE=report=<file>` appends them to
/// the file, with a line for each call checked.
fn reporting() -> Option<&'static Option<std::path::PathBuf>> {
    static REPORT: OnceLock<Option<Option<std::path::PathBuf>>> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            let value = std::env::var("SILT_EVIDENCE").ok()?;
            match value.strip_prefix("report")? {
                "" => Some(None),
                file => Some(Some(file.strip_prefix('=')?.into())),
            }
        })
        .as_ref()
}

/// Write `line` where the report goes.
fn report(line: &str) {
    match reporting() {
        Some(Some(file)) => {
            use std::io::Write as _;
            let opened = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file);
            if let Ok(mut file) = opened {
                let _ = file.write_all(format!("{line}\n").as_bytes());
            }
        }
        Some(None) => eprintln!("{line}"),
        None => {}
    }
}

/// When reporting: the class of the call at `ip` in the function
/// `function`, once for each call and class.
fn note(class: &str, function: &str, ip: usize) {
    static SEEN: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let line = format!("evidence-site {class} {function} {ip}");
    let seen = SEEN.get_or_init(Default::default);
    if seen.lock().is_ok_and(|mut seen| seen.insert(line.clone())) {
        let program: Vec<String> = std::env::args().skip(1).collect();
        report(&format!("{line} | {}", program.join(" ")));
    }
}

/// The level of the definition `owner` in `env`.
fn level(env: Option<&Arc<Env>>, owner: Span) -> Option<&Env> {
    let mut at = env.map(Arc::as_ref);
    while let Some(env) = at {
        if env.owner == owner {
            return Some(env);
        }
        at = env.parent.as_deref();
    }
    None
}

/// `ev` where the code sees `env`.
fn ground(ev: &Evidence, env: Option<&Arc<Env>>, globals: &Globals) -> Ground {
    match ev {
        Evidence::Impl { tr, ty, subs } => Ground::Impl {
            tr: *tr,
            ty: *ty,
            subs: Some(subs.iter().map(|sub| ground(sub, env, globals)).collect()),
        },
        Evidence::Param { owner, k } => level(env, *owner)
            .and_then(|level| level.grounds.as_ref()?.get(usize::from(*k)).cloned())
            .unwrap_or(Ground::Unknown),
        // The impl's own evidence of the supertrait, at the evidence of
        // its header; or the supertrait of the type, whatever has it.
        Evidence::Super { of, to } => match ground(of, env, globals) {
            Ground::Impl {
                tr,
                ty,
                subs: Some(subs),
            } if let Some(ev) = (globals.impl_evidence(tr, ty))
                .and_then(|impl_ev| impl_ev.supers.iter().find(|(s, _)| s == to)) =>
            {
                ground(&ev.1, Some(&header(subs)), globals)
            }
            Ground::Impl { ty, .. } | Ground::Super { ty, .. } | Ground::Native { ty, .. } => {
                Ground::Super { tr: *to, ty }
            }
            Ground::Structural(_) => Ground::Structural(*to),
            other => other,
        },
        Evidence::Structural { tr } => Ground::Structural(*tr),
        Evidence::Native { tr, ty } => Ground::Native { tr: *tr, ty: *ty },
        Evidence::Missing => Ground::Missing,
    }
}

/// The level of an impl's header: the evidence of its predicates.
fn header(subs: Arc<[Ground]>) -> Arc<Env> {
    Arc::new(Env {
        owner: crate::evidence::IMPL_HEADER,
        grounds: Some(subs.to_vec()),
        parent: None,
    })
}

/// The evidence that the impl `recv` names meets the `where` clauses
/// its trait declares on its parameters: a default method's last
/// bounds.
fn wheres(recv: &Ground, globals: &Globals) -> Vec<Ground> {
    let Ground::Impl {
        tr,
        ty,
        subs: Some(subs),
    } = recv
    else {
        return Vec::new();
    };
    let Some(impl_ev) = globals.impl_evidence(*tr, *ty) else {
        return Vec::new();
    };
    let header = header(subs.clone());
    (impl_ev.wheres.iter())
        .map(|ev| ground(ev, Some(&header), globals))
        .collect()
}

/// The closure `callee` with the level of its own bounds, which the
/// site `site` gives where the code sees `env`; `None` when it has it
/// already or is no definition with bounds.
fn bind(
    callee: &Arc<VmClosure>,
    site: &Site,
    env: Option<&Arc<Env>>,
    globals: &Globals,
) -> Option<Arc<VmClosure>> {
    let (owner, layout) = callee.function.evidence()?.owner?;
    if callee.ev.as_ref().is_some_and(|ev| ev.owner == owner) {
        return None;
    }
    let grounds = match (site, layout) {
        (Site::Ref(bounds), Layout::Scheme | Layout::ImplMethod) => {
            Some(bounds.iter().map(|ev| ground(ev, env, globals)).collect())
        }
        (Site::Ref(_), Layout::Default) => None,
        // (The impl for every function or tuple has the trait of its
        // `self` as its last bound: the receiver's evidence.)
        (Site::Method { recv, own }, Layout::ImplMethod) => match ground(recv, env, globals) {
            Ground::Impl {
                tr,
                ty,
                subs: Some(subs),
            } => {
                let mut grounds = subs.to_vec();
                grounds.extend(own.iter().map(|ev| ground(ev, env, globals)));
                grounds.push(Ground::Impl {
                    tr,
                    ty,
                    subs: Some(subs),
                });
                Some(grounds)
            }
            _ => None,
        },
        (Site::Method { recv, own }, Layout::Default) => {
            let recv = ground(recv, env, globals);
            let wheres = wheres(&recv, globals);
            let mut grounds = vec![recv];
            grounds.extend(own.iter().map(|ev| ground(ev, env, globals)));
            grounds.extend(wheres);
            Some(grounds)
        }
        (Site::Method { .. }, Layout::Scheme) => None,
    };
    let mut bound = VmClosure::clone(callee);
    bound.ev = Some(Arc::new(Env {
        owner,
        grounds,
        parent: callee.ev.clone(),
    }));
    Some(Arc::new(bound))
}

/// After a load by the code of `running` that ends at `ip`, the value
/// on top of `stack`: a closure of a definition with bounds gets the
/// level of its bounds, if the load passes evidence on.
pub(crate) fn bind_load(stack: &mut [Value], running: &VmClosure, ip: usize, globals: &Globals) {
    let Some(site) = running.function.evidence().and_then(|ev| ev.site(ip)) else {
        return;
    };
    let Some(Value::VmClosure(callee)) = stack.last() else {
        return;
    };
    if let Some(bound) = bind(callee, site, running.ev.as_ref(), globals)
        && let Some(top) = stack.last_mut()
    {
        *top = Value::VmClosure(bound);
    }
}

/// What the VM found for a call of a method.
pub(crate) struct Found<'a> {
    /// The trait of the call.
    pub(crate) tr: Option<TraitId>,
    /// The type of the receiver.
    pub(crate) ty: TypeId,
    /// Whether an impl has the method (else it is the VM's own).
    pub(crate) impl_found: bool,
    /// Whether the VM answers the call by the receiver's type's row of
    /// the builtin registry for the call's trait.
    pub(crate) native_row: bool,
    /// Whether the call is of a builtin trait's default the VM supplies
    /// (`message` of `Error`, for an impl that leaves it out): no impl
    /// has code for it.
    pub(crate) vm_default: bool,
    /// For a message: the method's name, the receiver.
    pub(crate) method: &'a str,
    pub(crate) receiver: &'a Value,
}

/// The name of the type `ty`, as a message of the check writes it.
fn type_name(ty: TypeId, found: &Found<'_>, globals: &Globals) -> String {
    if ty == found.ty {
        return crate::types::canonical::dispatch_type_name(found.receiver).to_string();
    }
    (globals.impl_type_name(ty).map(str::to_string))
        .or_else(|| (crate::defs::builtin_types().get(ty.0.0 as usize)).map(|(n, _)| n.to_string()))
        .unwrap_or_else(|| format!("type #{}", ty.0.0))
}

/// The name of the trait `tr`, as a message of the check writes it.
fn trait_name(tr: TraitId, globals: &Globals) -> String {
    (globals.trait_named(tr).map(str::to_string)).unwrap_or_else(|| format!("trait #{}", tr.0.0))
}

/// What the evidence `named` says, as a message of the check writes it.
fn shown(named: &Ground, found: &Found<'_>, globals: &Globals) -> String {
    match named {
        Ground::Impl { tr, ty, .. } => format!(
            "the impl of '{}' for '{}'",
            trait_name(*tr, globals),
            type_name(*ty, found, globals)
        ),
        Ground::Super { tr, ty } => format!(
            "supertrait '{}' of '{}'",
            trait_name(*tr, globals),
            type_name(*ty, found, globals)
        ),
        Ground::Structural(tr) => format!("'{}' by the type's structure", trait_name(*tr, globals)),
        Ground::Native { tr, ty } => format!(
            "the impl of '{}' the runtime provides for '{}'",
            trait_name(*tr, globals),
            type_name(*ty, found, globals)
        ),
        Ground::Missing => "nothing".to_string(),
        Ground::Unknown => "what the VM cannot work out".to_string(),
    }
}

/// Check the call of a method that the code of `running`, at the
/// instruction ending at `ip`, makes: an error when the evidence names
/// another impl than the one the VM found (unless reporting). The
/// closure the call runs, if it is one, with the level of its bounds.
pub(crate) fn check_call(
    running: &VmClosure,
    ip: usize,
    found: &Found<'_>,
    callee: Option<&Arc<VmClosure>>,
    globals: &Globals,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    let site = running.function.evidence().and_then(|ev| ev.site(ip));
    let Some(site @ Site::Method { recv, .. }) = site else {
        if reporting().is_some() {
            note("no-site", running.function.name(), ip);
        }
        return Ok(None);
    };
    let env = running.ev.as_ref();
    let named = ground(recv, env, globals);
    let agrees = match &named {
        Ground::Impl { tr, ty, .. } => {
            Some(Some(*tr) == found.tr && *ty == found.ty && (found.impl_found || found.vm_default))
        }
        Ground::Super { tr, ty } => Some(Some(*tr) == found.tr && *ty == found.ty),
        Ground::Structural(tr) => Some(Some(*tr) == found.tr && !found.impl_found),
        Ground::Native { tr, ty } => {
            Some(Some(*tr) == found.tr && *ty == found.ty && found.native_row)
        }
        Ground::Missing | Ground::Unknown => None,
    };
    if reporting().is_some() {
        // (A body the runtime calls, a `Display` impl that `println`
        // runs, a `json.Encode` impl that `json.stringify` runs, has no
        // evidence: no call of silt's passed any on.)
        let by_runtime = env.is_none()
            && (running.function.evidence())
                .and_then(|ev| ev.owner)
                .is_some_and(|(_, layout)| layout != Layout::Scheme);
        let class = match (&named, agrees) {
            (Ground::Missing, _) => "missing",
            (_, None) if by_runtime => "unchecked-by-runtime",
            (_, None) => "unchecked",
            (_, Some(true)) => "agrees",
            (_, Some(false)) => "disagrees",
        };
        note(class, running.function.name(), ip);
    }
    if agrees == Some(false) {
        let receiver = type_name(found.ty, found, globals);
        let tr = found
            .tr
            .map_or_else(String::new, |tr| trait_name(tr, globals));
        let what = match found.impl_found {
            true => format!("the impl of '{tr}' for '{receiver}'"),
            false => format!("no impl of '{tr}' for '{receiver}' (the VM's own method)"),
        };
        let message = format!(
            "evidence: the call of '{}' in '{}' (offset {ip}) found {what}, but the checker named {}",
            found.method,
            running.function.name(),
            shown(&named, found, globals)
        );
        if reporting().is_some() {
            let program: Vec<String> = std::env::args().skip(1).collect();
            report(&format!("{message} | {}", program.join(" ")));
        } else {
            return Err(VmError::type_confusion(message));
        }
    }
    Ok(callee.and_then(|callee| bind(callee, site, env, globals)))
}

/// Check, where a call through a trait is compiled, that the impl the
/// checker selected for it (`Selection::Impl`, or the VM's own method,
/// `Selection::Native`) is the one its evidence names: the message of a
/// disagreement, unless reporting, which writes it.
pub(crate) fn check_selection(
    selection: crate::ast::Selection,
    site: Option<&Site>,
    function: &str,
    at: Span,
) -> Result<(), String> {
    let at = at.start as usize;
    use crate::ast::Selection;
    let Some(Site::Method { recv, .. }) = site else {
        return Ok(());
    };
    let agrees = match (selection, recv) {
        (Selection::Impl { tr, ty }, Evidence::Impl { tr: t, ty: y, .. }) => tr == *t && ty == *y,
        (Selection::Native { tr }, Evidence::Structural { tr: t }) => tr == *t,
        // (The select pass names the impl the runtime provides as the
        // type's impl; it has no code, and the call is the VM's.)
        (Selection::Impl { tr, ty }, Evidence::Native { tr: t, ty: y }) => tr == *t && ty == *y,
        (Selection::Impl { .. } | Selection::Native { .. }, Evidence::Missing) => {
            if reporting().is_some() {
                note("missing-static", function, at);
            }
            return Ok(());
        }
        (Selection::Impl { .. } | Selection::Native { .. }, _) => false,
        _ => return Ok(()),
    };
    if reporting().is_some() {
        note(
            if agrees {
                "agrees-static"
            } else {
                "disagrees-static"
            },
            function,
            at,
        );
    }
    if agrees {
        return Ok(());
    }
    let message = format!(
        "evidence: a call through a trait in '{function}' selects {selection:?}, but the checker named {recv:?}"
    );
    match reporting() {
        Some(_) => {
            let program: Vec<String> = std::env::args().skip(1).collect();
            report(&format!("{message} | {}", program.join(" ")));
            Ok(())
        }
        None => Err(message),
    }
}

/// The closure the code of `running` makes: it sees the code's
/// evidence.
pub(crate) fn made_in(running: &VmClosure, mut made: VmClosure) -> VmClosure {
    made.ev = running.ev.clone();
    made
}

impl super::Vm {
    /// Check the `CallMethod` being run (its frame's `ip` saved), of
    /// the method with the id `method` of the trait `trait_index`
    /// names, on the receiver in the stack slot `receiver`, which found the global
    /// slot `found` (none: the VM's own method). The closure in that
    /// slot with the level of its bounds, when the call gives it one.
    pub(super) fn evidence_call(
        &self,
        trait_index: u16,
        receiver: usize,
        method: u16,
        found: Option<u16>,
    ) -> Result<Option<Arc<VmClosure>>, VmError> {
        let receiver = &self.stack[receiver];
        let ty = crate::types::canonical::dispatch_type_for_value(receiver);
        let frame = self.frame();
        let callee = match found.and_then(|slot| self.view.global(slot)) {
            Some(Value::VmClosure(callee)) => Some(callee),
            _ => None,
        };
        let entry = &self.view.entry;
        let tr = entry.trait_at(trait_index);
        let method = entry.method_name(method).unwrap_or("?");
        // (The message of a builtin module's error enum is its row's;
        // that of an `Error` impl that leaves it out, the value's
        // display, a builtin trait's default the VM supplies.)
        let message_of_error =
            found.is_none() && method == "message" && tr == crate::defs::builtin_trait_id("Error");
        let native_row = message_of_error
            && crate::builtins::registry::registry()
                .error_module(ty)
                .and_then(|module| module.message.as_ref())
                .is_some_and(|row| row.enabled);
        let found = Found {
            tr,
            ty,
            impl_found: found.is_some(),
            native_row,
            vm_default: message_of_error && !native_row,
            method,
            receiver,
        };
        check_call(&frame.closure, frame.ip, &found, callee, entry)
    }
}

#[cfg(test)]
mod tests;
