use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

pub(crate) struct CollectionAdmission {
    state: Arc<AtomicU64>,
}

impl CollectionAdmission {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn generation(&self) -> Option<u64> {
        let state = self.state.load(Ordering::Acquire);
        (state & 1 != 0).then_some(state)
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.generation() == Some(generation)
    }

    pub(crate) fn admit(&self) -> Option<Permit> {
        self.generation().map(|generation| Permit {
            state: self.state.clone(),
            generation,
        })
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> bool {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if (current & 1 != 0) == enabled {
                return false;
            }
            let next = (current.wrapping_add(2) & !1) | u64::from(enabled);
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Permit {
    state: Arc<AtomicU64>,
    generation: u64,
}

thread_local! {
    static CURRENT: RefCell<Option<Permit>> = const { RefCell::new(None) };
}

impl Permit {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn is_current(&self) -> bool {
        self.state.load(Ordering::Acquire) == self.generation
    }

    // Blocking provider work stays on this thread. No permit crosses an await.
    pub(crate) fn run<T>(&self, work: impl FnOnce() -> T) -> Option<T> {
        if !self.is_current() {
            return None;
        }
        struct Restore(Option<Permit>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(CURRENT.with(|slot| slot.replace(Some(self.clone()))));
        Some(work())
    }
}

pub(crate) fn current_generation() -> Option<u64> {
    CURRENT.with(|slot| slot.borrow().as_ref().map(Permit::generation))
}

pub(crate) fn current_is_allowed() -> bool {
    CURRENT.with(|slot| slot.borrow().as_ref().is_none_or(Permit::is_current))
}

#[derive(Debug)]
pub(crate) struct PolicyCancelled;

impl std::fmt::Display for PolicyCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("command unavailable by collection policy")
    }
}
impl std::error::Error for PolicyCancelled {}

pub(crate) fn ensure_current() -> Result<(), PolicyCancelled> {
    if current_is_allowed() {
        Ok(())
    } else {
        Err(PolicyCancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_reenabled_policies_retire_queued_work() {
        let policy = CollectionAdmission::new();
        assert_eq!(policy.generation(), None);
        assert!(policy.set_enabled(true));
        let queued = policy.generation().unwrap();
        assert!(!policy.set_enabled(true));
        assert!(policy.is_current(queued));
        assert!(policy.set_enabled(false));
        assert!(!policy.is_current(queued));
        assert!(policy.set_enabled(true));
        assert!(!policy.is_current(queued));
    }

    #[test]
    fn nested_scopes_restore_the_previous_permit_even_after_panic() {
        let outer = CollectionAdmission::new();
        let inner = CollectionAdmission::new();
        outer.set_enabled(true);
        inner.set_enabled(true);
        let outer_permit = outer.admit().unwrap();
        outer_permit.run(|| {
            let _ = std::panic::catch_unwind(|| {
                inner.admit().unwrap().run(|| {
                    inner.set_enabled(false);
                    assert!(!current_is_allowed());
                    panic!("synthetic provider failure");
                })
            });
            assert!(current_is_allowed());
            outer.set_enabled(false);
            assert!(!current_is_allowed());
        });
        assert!(current_is_allowed());
        assert_eq!(current_generation(), None);
        assert!(outer_permit
            .run(|| panic!("retired work started"))
            .is_none());
    }
}
