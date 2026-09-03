//! Which protocol owns the screen.
//!
//! The Pi speaks two protocols and has one display. Whoever connects first
//! owns it until they disconnect; the other is refused with a clear message.
//! Neither can preempt the other, so a guest presenting cannot be knocked off
//! by a background reconnect, and vice versa.

use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Idle,
    Castr,
    // Taken by the sink, which lands in the next task.
    #[allow(dead_code)]
    Miracast,
}

pub struct DisplayArbiter {
    owner: Mutex<Owner>,
}

impl Default for DisplayArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayArbiter {
    pub fn new() -> Self {
        Self {
            owner: Mutex::new(Owner::Idle),
        }
    }

    pub fn owner(&self) -> Owner {
        *self.owner.lock().unwrap()
    }

    /// Grants the display when it is free, or already held by `who`.
    pub fn try_acquire(&self, who: Owner) -> bool {
        let mut o = self.owner.lock().unwrap();
        if *o == Owner::Idle || *o == who {
            *o = who;
            true
        } else {
            false
        }
    }

    /// Releases the display, but only for the owner that holds it.
    pub fn release(&self, who: Owner) {
        let mut o = self.owner.lock().unwrap();
        if *o == who {
            *o = Owner::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn an_idle_display_is_granted_to_whoever_asks_first() {
        let a = DisplayArbiter::new();
        assert_eq!(a.owner(), Owner::Idle);
        assert!(a.try_acquire(Owner::Castr));
        assert_eq!(a.owner(), Owner::Castr);
    }

    #[test]
    fn the_second_protocol_is_refused_until_the_first_releases() {
        let a = DisplayArbiter::new();
        assert!(a.try_acquire(Owner::Miracast));
        assert!(!a.try_acquire(Owner::Castr), "castr is refused");
        a.release(Owner::Miracast);
        assert_eq!(a.owner(), Owner::Idle);
        assert!(a.try_acquire(Owner::Castr));
    }

    #[test]
    fn acquiring_twice_from_the_same_owner_succeeds_and_release_is_idempotent() {
        let a = DisplayArbiter::new();
        assert!(a.try_acquire(Owner::Castr));
        assert!(a.try_acquire(Owner::Castr), "reentrant for the same owner");
        a.release(Owner::Castr);
        a.release(Owner::Castr);
        assert_eq!(a.owner(), Owner::Idle);
    }

    #[test]
    fn releasing_from_the_wrong_owner_does_nothing() {
        let a = DisplayArbiter::new();
        a.try_acquire(Owner::Castr);
        a.release(Owner::Miracast);
        assert_eq!(
            a.owner(),
            Owner::Castr,
            "a stale release cannot steal the display"
        );
    }

    #[test]
    fn it_is_shareable_across_threads() {
        let a = Arc::new(DisplayArbiter::new());
        let b = a.clone();
        let t = std::thread::spawn(move || b.try_acquire(Owner::Miracast));
        let first = t.join().unwrap();
        let second = a.try_acquire(Owner::Castr);
        assert!(first ^ second, "exactly one of the two holds it");
    }
}
