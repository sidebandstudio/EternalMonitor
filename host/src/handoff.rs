//! An update and "Restart EternalMonitor" both start another copy and close
//! this one, so only one may run at a time. The update helper waits for every
//! copy to exit before it installs: a restart copy started meanwhile would
//! become the running app and the install would be skipped. A restart
//! started while an update hands over would leave a copy the helper waits on.
//!
//! The process has one [`Gate`], shared by the updater and the window's
//! restart. Both claim it on the GUI thread (the updater from its frame
//! tick), so a check made in a click handler still holds when that handler
//! claims it.

use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    Update,
    Restart,
}

impl Handoff {
    /// The line shown where the user asked for the other one.
    pub fn refusal(self) -> &'static str {
        match self {
            Self::Update => "An update is installing; try again in a minute.",
            Self::Restart => "EternalMonitor is restarting.",
        }
    }
}

#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<Option<Handoff>>>);

impl Gate {
    /// Takes the gate for `handoff`, or says which handoff holds it.
    pub fn claim(&self, handoff: Handoff) -> Result<(), Handoff> {
        let mut held = self.0.lock();
        if let Some(holder) = *held {
            return Err(holder);
        }
        *held = Some(handoff);
        Ok(())
    }

    /// `handoff` ended and this copy stays open.
    pub fn release(&self, handoff: Handoff) {
        let mut held = self.0.lock();
        if *held == Some(handoff) {
            *held = None;
        }
    }

    pub fn holder(&self) -> Option<Handoff> {
        *self.0.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_handoff_at_a_time_in_either_order() {
        let gate = Gate::default();
        gate.claim(Handoff::Update).unwrap();
        assert_eq!(gate.claim(Handoff::Restart), Err(Handoff::Update));
        assert_eq!(gate.claim(Handoff::Update), Err(Handoff::Update));
        // Only the holder releases it.
        gate.release(Handoff::Restart);
        assert_eq!(gate.holder(), Some(Handoff::Update));
        gate.release(Handoff::Update);
        gate.claim(Handoff::Restart).unwrap();
        assert_eq!(gate.clone().claim(Handoff::Update), Err(Handoff::Restart));
        gate.release(Handoff::Restart);
        assert_eq!(gate.holder(), None);
    }
}
