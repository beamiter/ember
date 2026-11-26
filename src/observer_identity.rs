//! Observer targets are capabilities for one generation, never an active-pane lookup.
use std::sync::{Arc, Weak};
pub(crate) struct Target<T> {
    id: String,
    generation: Weak<T>,
}
impl<T> Target<T> {
    pub(crate) fn new(id: String, owner: &Arc<T>) -> Self {
        Self {
            id,
            generation: Arc::downgrade(owner),
        }
    }
    pub(crate) fn matches(&self, id: &str, owner: &Arc<T>) -> bool {
        self.id == id
            && Weak::ptr_eq(&self.generation, &Arc::downgrade(owner))
            && self.generation.strong_count() > 0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_generation_and_identity_required() {
        let a = Arc::new(());
        let b = Arc::new(());
        let t = Target::new("a".into(), &a);
        assert!(t.matches("a", &a));
        assert!(!t.matches("b", &a));
        assert!(!t.matches("a", &b));
    }
    #[test]
    fn observer_does_not_retain_owner() {
        let a = Arc::new(());
        let weak = Arc::downgrade(&a);
        let _t = Target::new("a".into(), &a);
        assert_eq!(Arc::strong_count(&a), 1);
        drop(a);
        assert!(weak.upgrade().is_none());
    }
}
