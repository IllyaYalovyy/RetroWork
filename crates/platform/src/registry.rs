//! Module-owned registrations that are committed with activation and removed at shutdown.

use crate::{ActivationContext, ActivationError, ModuleId, ModuleResource};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// A registered value and the module responsible for it.
struct Entry<T> {
    owner: ModuleId,
    value: T,
}

/// A named registry owned by one module and extended by its dependents.
pub struct Registry<T: Clone + 'static> {
    owner: ModuleId,
    entries: Rc<RefCell<BTreeMap<String, Entry<T>>>>,
}
impl<T: Clone + 'static> Registry<T> {
    /// Creates an empty registry owned by `owner`.
    pub fn new(owner: ModuleId) -> Self {
        Self {
            owner,
            entries: Rc::default(),
        }
    }

    /// Stages one value. Duplicate keys or undeclared registry dependencies fail activation.
    pub fn register(
        &self,
        cx: &mut ActivationContext<'_>,
        key: &str,
        value: T,
    ) -> Result<(), ActivationError> {
        if key.is_empty() || self.entries.borrow().contains_key(key) {
            return Err(ActivationError::new(format!(
                "duplicate or empty registration key `{key}`"
            )));
        }
        let entries = Rc::clone(&self.entries);
        let key = key.to_owned();
        let contributor = cx.module().clone();
        cx.stage_registration(self.owner.clone(), &key.clone(), move || {
            entries.borrow_mut().insert(
                key.clone(),
                Entry {
                    owner: contributor,
                    value,
                },
            );
            Rc::new(Registration { entries, key })
        })
    }

    /// Looks up a value without exposing mutable registry state.
    pub fn get(&self, key: &str) -> Option<T> {
        self.entries
            .borrow()
            .get(key)
            .map(|entry| entry.value.clone())
    }

    /// Returns values in key order.
    pub fn values(&self) -> Vec<T> {
        self.entries
            .borrow()
            .values()
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// Identifies the module that contributed a value.
    pub fn contributor(&self, key: &str) -> Option<ModuleId> {
        self.entries
            .borrow()
            .get(key)
            .map(|entry| entry.owner.clone())
    }
}

/// Removes a committed registration while its registry is still alive.
struct Registration<T> {
    entries: Rc<RefCell<BTreeMap<String, Entry<T>>>>,
    key: String,
}
impl<T> ModuleResource for Registration<T> {
    fn shutdown(&self) {
        self.entries.borrow_mut().remove(&self.key);
    }
}
