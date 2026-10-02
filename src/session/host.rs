//! Host modules: modules an embedder declares to the session, whose
//! functions are Rust closures (design decision D7).
//!
//! A host module is imported like any module (`import mylib`). Each of
//! its functions is declared by its signature, `fn double(x: Int) ->
//! Int`, which the checker reads as it reads a module's `pub fn`; the
//! closure is what a call runs.
//!
//! ```rust
//! use silt::session::HostModule;
//!
//! let mylib = HostModule::new("mylib")
//!     .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
//!     .fn2("fn greet(name: String, n: Int) -> String", |name: String, n: i64| {
//!         format!("hello {name} x{n}")
//!     });
//! ```

use std::fmt;
use std::sync::Arc;

use crate::value::{FromValue, HostImpl, IntoValue, Value};
use crate::vm::VmError;

/// A module of Rust functions, declared to a session in
/// [`super::Config::host`].
#[derive(Clone)]
pub struct HostModule {
    /// The name programs import the module by.
    pub name: String,
    pub fns: Vec<HostFunction>,
}

/// One function of a [`HostModule`].
#[derive(Clone)]
pub struct HostFunction {
    /// The function's signature, a `fn` header with typed parameters and
    /// return type and no body: `fn double(x: Int) -> Int`. Its name is
    /// the function's name in the module.
    pub signature: String,
    /// What a call runs. It is given arguments of the declared types.
    pub call: HostImpl,
}

impl HostModule {
    /// A module named `name`, with no functions yet.
    pub fn new(name: impl Into<String>) -> HostModule {
        HostModule {
            name: name.into(),
            fns: Vec::new(),
        }
    }

    /// Add the function declared by `signature`, called with the
    /// arguments as values.
    ///
    /// A panic inside `call` becomes a runtime error that names the
    /// function; returning `Err` is still the way to fail.
    pub fn function(
        mut self,
        signature: impl Into<String>,
        call: impl Fn(&[Value]) -> Result<Value, VmError> + Send + Sync + 'static,
    ) -> HostModule {
        self.fns.push(HostFunction {
            signature: signature.into(),
            call: Arc::new(call),
        });
        self
    }

    /// Add a function of no arguments, its result converted to a value.
    pub fn fn0<R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn() -> R + Send + Sync + 'static,
    ) -> HostModule {
        self.function(signature, move |_: &[Value]| {
            f().into_value().map_err(VmError::new)
        })
    }

    /// Add a function of one argument, converted from and to values.
    pub fn fn1<A: FromValue, R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn(A) -> R + Send + Sync + 'static,
    ) -> HostModule {
        self.function(signature, move |args: &[Value]| {
            let a = A::from_value(&args[0]).map_err(VmError::new)?;
            f(a).into_value().map_err(VmError::new)
        })
    }

    /// Add a function of two arguments, converted from and to values.
    pub fn fn2<A: FromValue, B: FromValue, R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn(A, B) -> R + Send + Sync + 'static,
    ) -> HostModule {
        self.function(signature, move |args: &[Value]| {
            let a = A::from_value(&args[0]).map_err(|e| VmError::new(format!("arg 1: {e}")))?;
            let b = B::from_value(&args[1]).map_err(|e| VmError::new(format!("arg 2: {e}")))?;
            f(a, b).into_value().map_err(VmError::new)
        })
    }

    /// The text the module is checked from: one signature per line.
    pub(super) fn signatures(&self) -> String {
        let mut text = String::new();
        for f in &self.fns {
            text.push_str(&f.signature);
            text.push('\n');
        }
        text
    }
}

impl fmt::Debug for HostModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostModule")
            .field("name", &self.name)
            .field(
                "fns",
                &self.fns.iter().map(|f| &f.signature).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl fmt::Debug for HostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFunction({})", self.signature)
    }
}
