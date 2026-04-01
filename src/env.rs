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
}

impl Env {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                bindings: HashMap::new(),
            })),
        }
    }

    pub fn child(&self) -> Self {
        let parent_bindings = self.inner.borrow().bindings.clone();
        Self {
            inner: Rc::new(RefCell::new(EnvInner {
                bindings: parent_bindings,
            })),
        }
    }

    pub fn define(&self, name: String, value: Value) {
        self.inner.borrow_mut().bindings.insert(name, value);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.inner.borrow().bindings.get(name).cloned()
    }

    /// Return all bindings whose key starts with `prefix`.
    pub fn bindings_with_prefix(&self, prefix: &str) -> Vec<(String, Value)> {
        self.inner
            .borrow()
            .bindings
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
