// https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event

use std::cell::Cell;
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct LoadDelay {
    pending: Rc<Cell<usize>>,
}

impl LoadDelay {
    pub(crate) fn guard(&self) -> LoadDelayGuard {
        self.pending.set(self.pending.get() + 1);
        LoadDelayGuard {
            pending: Rc::clone(&self.pending),
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.get() != 0
    }
}

pub(crate) struct LoadDelayGuard {
    pending: Rc<Cell<usize>>,
}

impl Drop for LoadDelayGuard {
    fn drop(&mut self) {
        self.pending.set(self.pending.get() - 1);
    }
}
