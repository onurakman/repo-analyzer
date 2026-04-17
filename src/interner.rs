use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Thread-safe string interner that returns shared `Arc<str>` for repeated values.
///
/// Used for high-repetition fields like file paths and author emails — each unique
/// string is allocated once and every subsequent occurrence is a cheap refcount
/// increment instead of a full string clone. For a 32k-commit repo with thousands
/// of files this trades a small Mutex contention cost for hundreds of MB of saved
/// allocations across collectors.
#[derive(Debug, Default)]
pub struct Interner {
    set: Mutex<HashSet<Arc<str>>>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a shared `Arc<str>` for `s`, allocating only once per unique value.
    pub fn intern(&self, s: &str) -> Arc<str> {
        let mut g = self.set.lock().expect("interner mutex poisoned");
        if let Some(existing) = g.get(s) {
            return existing.clone();
        }
        let arc: Arc<str> = Arc::from(s);
        g.insert(arc.clone());
        arc
    }

    /// Number of unique strings currently held — useful for diagnostics.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.set.lock().map(|g| g.len()).unwrap_or(0)
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_same_value_to_same_arc() {
        let i = Interner::new();
        let a = i.intern("hello");
        let b = i.intern("hello");
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn distinct_values_get_distinct_arcs() {
        let i = Interner::new();
        let a = i.intern("foo");
        let b = i.intern("bar");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn deref_works() {
        let i = Interner::new();
        let a = i.intern("hello");
        assert_eq!(&*a, "hello");
    }
}
