//! `task.guard`: run a function, and give a runtime error it raises
//! as a value, a `task.RuntimeError`.
//!
//! The function runs in place, on the caller's task: the guard is a frame
//! of the VM ([`Guard`]), and a runtime error raised above it, in the
//! function or in what it calls, ends the frames above the guard as an
//! error ends a task's (each builtin's frame is told: `Native::abandon`)
//! and the guard's call with `Err(e)`. The two loops that run a VM's
//! frames catch it there (`Vm::caught`, src/vm/run.rs). No task is
//! started for it.
//!
//! What ends what it is raised in is not caught (`VmError::catchable`).
//! Nor is a stop: a task that was cancelled, or whose deadline passed,
//! ends where it is, with no error, and its guards with it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::source::Span;
use crate::typeinfo::{bv, ty};
use crate::value::Value;
use crate::value::{Record, RecordId};
use crate::vm::{FileView, Native, Step, Vm, VmError};

/// The frame of `task.guard(work)`. Its value is `Ok` of what `work`
/// returned; a runtime error raised above it finishes it with `Err(e)`
/// instead (`Vm::caught`).
pub(crate) struct Guard {
    /// The function, until it is called.
    work: Option<Value>,
    /// The height of the stack under the frame: what an error caught
    /// here cuts the stack back to.
    stack: usize,
}

impl Guard {
    /// The frame of `task.guard(work)`, with the stack of `vm` as it is
    /// under the call. It is counted (`Vm::guards`) until it is done or
    /// dropped.
    pub(crate) fn enter(vm: &mut Vm, work: Value) -> Step {
        vm.guards += 1;
        Step::Run(Box::new(Guard {
            work: Some(work),
            stack: vm.stack.len(),
        }))
    }
}

impl Native for Guard {
    fn name(&self) -> &str {
        "task.guard"
    }

    fn guard(&self) -> Option<usize> {
        Some(self.stack)
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(work) = self.work.take() {
            return Ok(vm.call(work, []));
        }
        vm.guards -= 1;
        Ok(Step::Done(Value::variant(bv::OK, [input])))
    }

    /// An error passes through: one that ends what it is raised in, or
    /// the VM is dropped.
    fn abandon(&mut self, vm: &mut Vm) {
        vm.guards -= 1;
    }
}

/// The `task.RuntimeError` of `error`, which has its place and its call
/// stack (`Vm::enrich_error`): its message; its place and the lines of
/// its call stack as the report of the error names them, with the
/// program's files named as `vm` names them (`Vm::files`). Shown, it
/// is that report ([`write_report`]), which is written when it is first
/// shown: an error that is caught and dropped costs no report.
pub(crate) fn runtime_error(vm: &Vm, error: VmError) -> Value {
    let files = vm.files();
    let location = files.frame(error.span.unwrap_or(Span::BUILTIN));
    let lines =
        crate::vm::error::render_call_stack(&error.call_stack, |_, span| files.frame(*span));
    let trace = lines.iter().map(|line| {
        let entry = line
            .strip_prefix("  -> ")
            .or_else(|| line.strip_prefix("  "));
        Value::string(entry.unwrap_or(line))
    });
    let value = Value::builtin_record(
        ty::RUNTIME_ERROR,
        [
            ("message", Value::string(error.message.as_str())),
            ("location", Value::string(location)),
            ("trace", Value::list(trace.collect())),
        ],
    );
    if let Value::Record(record) = &value {
        // The report of an error whose call stack it cuts short is
        // written now: the error holds a line for every frame it was
        // raised under, a million or more for a recursion that ran out
        // of the frame budget, and the table keeps a report until its
        // record has gone and the table is swept.
        let cut = lines.iter().any(|line| line.starts_with("  ... ("));
        let report = match cut {
            true => Report::Text(
                error
                    .render_in(&*files, &crate::diagnostic::COLORS_OFF)
                    .into(),
            ),
            false => Report::Due(Box::new(error), files),
        };
        Reports::keep(record, report);
    }
    value
}

/// How many lines of call stacks the reports not written yet hold, in
/// every program of the process: what the table keeps of the errors
/// themselves.
#[cfg(test)]
pub(crate) fn frames_kept() -> usize {
    REPORTS
        .lock()
        .by_record
        .values()
        .map(|(_, report)| match report {
            Report::Due(error, _) => error.call_stack.len(),
            Report::Text(_) => 0,
        })
        .sum()
}

/// The report of an error `task.guard` caught.
#[derive(Clone)]
enum Report {
    /// Not written yet: the error, and how the program's files were
    /// named when it was caught.
    Due(Box<VmError>, FileView),
    /// Written.
    Text(Arc<str>),
}

/// The reports of the `task.RuntimeError`s `task.guard` made, by the
/// record (and its clones): what showing one writes. It has the
/// excerpt of the source, which its fields do not hold. A record of the
/// type that a program made, or changed (`e.{ message: .. }`), is
/// another record, which is shown from its fields ([`write_report`]).
/// An entry goes when its record has gone: the table is swept when it
/// has grown to twice what it held after the last sweep.
#[derive(Default)]
struct Reports {
    by_record: HashMap<usize, (RecordId, Report)>,
    /// How many entries the table may hold before it is swept again.
    sweep_at: usize,
}

static REPORTS: std::sync::LazyLock<parking_lot::Mutex<Reports>> =
    std::sync::LazyLock::new(Default::default);

impl Reports {
    /// The fewest entries the table holds before it is swept.
    const LEAST: usize = 64;

    fn keep(record: &Record, report: Report) {
        let mut reports = REPORTS.lock();
        if reports.by_record.len() >= reports.sweep_at.max(Reports::LEAST) {
            reports.by_record.retain(|_, (id, _)| !id.is_gone());
            reports.sweep_at = 2 * reports.by_record.len();
        }
        let id = record.id();
        reports.by_record.insert(id.key(), (id, report));
    }

    /// The report of `record`, written now if it was not (outside the
    /// table's lock: naming a file can read the file system).
    fn of(record: &Record) -> Option<Arc<str>> {
        let key = record.id().key();
        let found = {
            let reports = REPORTS.lock();
            let (id, report) = reports.by_record.get(&key)?;
            record.is(id).then(|| report.clone())?
        };
        let (error, files) = match found {
            Report::Text(text) => return Some(text),
            Report::Due(error, files) => (error, files),
        };
        let text: Arc<str> = error
            .render_in(&*files, &crate::diagnostic::COLORS_OFF)
            .into();
        if let Some((id, report)) = REPORTS.lock().by_record.get_mut(&key)
            && record.is(id)
        {
            *report = Report::Text(text.clone());
        }
        Some(text)
    }
}

/// Write `record` if it is a `task.RuntimeError`, as the report of its
/// error is printed, and say so. One that `task.guard` made is its
/// report (the place's source excerpt included); any other is written
/// from its fields: its message, its place, and its call stack.
pub(crate) fn write_report(
    record: &Record,
    f: &mut std::fmt::Formatter<'_>,
) -> Result<bool, std::fmt::Error> {
    if record.type_id() != ty::RUNTIME_ERROR {
        return Ok(false);
    }
    if let Some(report) = Reports::of(record) {
        f.write_str(&report)?;
        return Ok(true);
    }
    let text = |name: &str| match record.get(name) {
        Some(Value::String(s, _)) => s.to_string(),
        _ => String::new(),
    };
    write!(
        f,
        "error[runtime]: {}\n --> {}",
        text("message"),
        text("location")
    )?;
    if let Some(Value::List(trace)) = record.get("trace")
        && !trace.is_empty()
    {
        f.write_str("\n\ncall stack:")?;
        for entry in trace.iter() {
            let entry = match entry {
                Value::String(s, _) => s.to_string(),
                other => other.to_string(),
            };
            match entry.starts_with("... (") {
                true => write!(f, "\n  {entry}")?,
                false => write!(f, "\n  -> {entry}")?,
            }
        }
    }
    Ok(true)
}
