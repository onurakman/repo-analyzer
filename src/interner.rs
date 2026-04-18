use std::sync::Arc;

use dashmap::DashSet;

/// Thread-safe string interner that returns shared `Arc<str>` for repeated values.
///
/// Used for high-repetition fields like file paths and author emails — each unique
/// string is allocated once and every subsequent occurrence is a cheap refcount
/// increment instead of a full string clone. For a 32k-commit repo with thousands
/// of files this saves hundreds of MB of duplicated allocations.
///
/// Backed by `DashSet` (a sharded concurrent set) so intern hits from rayon
/// workers go through a striped lock instead of contending on one global Mutex.
#[derive(Debug, Default)]
pub struct Interner {
    set: DashSet<Arc<str>>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a shared `Arc<str>` for `s`, allocating only once per unique value.
    ///
    /// Fast path: a read-only `get` on the shard — no write lock. Slow path
    /// allocates an `Arc` and inserts. If two threads race on the slow path,
    /// the loser re-fetches the winner's canonical `Arc`, which is cheap.
    pub fn intern(&self, s: &str) -> Arc<str> {
        if let Some(existing) = self.set.get(s) {
            return existing.clone();
        }
        let arc: Arc<str> = Arc::from(s);
        if self.set.insert(arc.clone()) {
            // We won the race; `arc` is now the canonical entry.
            arc
        } else {
            // Another thread inserted first — return their canonical `Arc`.
            self.set.get(s).map(|e| e.clone()).unwrap_or(arc)
        }
    }

    /// Number of unique strings currently held — useful for diagnostics.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
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

    #[test]
    fn parallel_intern_converges_to_single_arc() {
        // Hammer the interner from many threads on the same value and verify
        // every thread ended up with pointer-equal Arcs (i.e. there's one
        // canonical allocation, regardless of the insert race).
        use std::sync::Arc as StdArc;
        use std::thread;

        let i = StdArc::new(Interner::new());
        let mut handles = Vec::new();
        for _ in 0..16 {
            let i = i.clone();
            handles.push(thread::spawn(move || {
                (0..200).map(|_| i.intern("hot-key")).collect::<Vec<_>>()
            }));
        }
        let mut canonical: Option<Arc<str>> = None;
        for h in handles {
            for arc in h.join().expect("thread panicked") {
                match &canonical {
                    Some(c) => assert!(Arc::ptr_eq(c, &arc)),
                    None => canonical = Some(arc),
                }
            }
        }
        assert_eq!(i.len(), 1);
    }
}
