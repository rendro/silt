use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Env {
    inner: Rc<RefCell<EnvInner>>,
}

#[derive(Debug)]
struct EnvInner {
    bindings: HashMap<String, Value>,
    parent: Option<Env>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                bindings: HashMap::new(),
                parent: None,
            })),
        }
    }

    pub fn child(&self) -> Self {
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                bindings: HashMap::new(),
                parent: Some(self.clone()),
            })),
        }
    }

    pub fn define(&self, name: String, value: Value) {
        self.inner.borrow_mut().bindings.insert(name, value);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        let inner = self.inner.borrow();
        if let Some(val) = inner.bindings.get(name) {
            Some(val.clone())
        } else if let Some(ref parent) = inner.parent {
            parent.get(name)
        } else {
            None
        }
    }

    /// Return all bindings (including parent scopes) whose key starts with `prefix`.
    pub fn bindings_with_prefix(&self, prefix: &str) -> Vec<(String, Value)> {
        let mut results = HashMap::new();
        self.collect_with_prefix(prefix, &mut results);
        results.into_iter().collect()
    }

    fn collect_with_prefix(&self, prefix: &str, out: &mut HashMap<String, Value>) {
        let inner = self.inner.borrow();
        if let Some(ref parent) = inner.parent {
            parent.collect_with_prefix(prefix, out);
        }
        for (k, v) in &inner.bindings {
            if k.starts_with(prefix) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
}
