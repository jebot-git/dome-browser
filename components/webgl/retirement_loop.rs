// SPDX-License-Identifier: MPL-2.0
//! Keep deferred native owners on their original thread through shutdown.
use std::time::{Duration, Instant};

pub(crate) enum Input<M> {
    Message(M),
    Timeout,
    Disconnected,
}
pub(crate) trait Worker {
    type Message;
    fn receive(&mut self, timeout: Option<Duration>) -> Input<Self::Message>;
    /// True requests shutdown. Once closing, accept only cleanup messages.
    fn handle(&mut self, message: Self::Message, closing: bool) -> bool;
    fn pending(&self) -> bool;
    /// Retry owned resources; return true only when all shutdown owners are gone.
    fn retire(&mut self, closing: bool) -> bool;
}
pub(crate) fn run(worker: &mut impl Worker) {
    let mut closing = false;
    let mut disconnected = false;
    let mut delay = Duration::from_millis(10);
    let mut next_retry = Instant::now();
    loop {
        let pending = closing || worker.pending();
        let timeout = pending.then(|| next_retry.saturating_duration_since(Instant::now()));
        let input = if disconnected {
            // A disconnected receiver returns immediately. Sleep instead of spinning.
            std::thread::sleep(timeout.unwrap_or(delay));
            Input::Timeout
        } else {
            worker.receive(timeout)
        };
        match input {
            Input::Message(message) => {
                if worker.handle(message, closing) {
                    closing = true;
                    next_retry = Instant::now();
                }
            },
            Input::Disconnected => {
                disconnected = true;
                closing = true;
                next_retry = Instant::now();
            },
            Input::Timeout => {},
        }
        if (closing || worker.pending()) && Instant::now() >= next_retry {
            if worker.retire(closing) {
                if closing {
                    return;
                }
                delay = Duration::from_millis(10);
            } else {
                delay = (delay * 2).min(Duration::from_millis(500));
            }
            next_retry = Instant::now() + delay;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, rc::Rc};
    struct Owner {
        // Deliberately !Send, as with native consumer textures.
        native: Rc<()>,
        events: VecDeque<Input<bool>>,
        attempts: usize,
        idle_pending: bool,
        ordinary_messages: usize,
        closing_seen: bool,
    }
    impl Worker for Owner {
        type Message = bool;
        fn receive(&mut self, timeout: Option<Duration>) -> Input<bool> {
            let event = self.events.pop_front().unwrap_or(Input::Timeout);
            if matches!(event, Input::Timeout) {
                assert!(timeout.is_some());
                std::thread::sleep(timeout.unwrap());
            }
            event
        }

        fn handle(&mut self, stop: bool, closing: bool) -> bool {
            if !closing && !stop {
                self.ordinary_messages += 1;
            }
            stop
        }
        fn pending(&self) -> bool {
            self.idle_pending
        }
        fn retire(&mut self, closing: bool) -> bool {
            assert_eq!(Rc::strong_count(&self.native), 1);
            self.closing_seen |= closing;
            self.attempts += 1;
            let complete = self.attempts >= 3;
            if complete {
                self.idle_pending = false;
            }
            complete
        }
    }
    #[test]
    fn exit_retains_local_owner_and_retries_before_returning() {
        let mut owner = Owner {
            native: Rc::new(()),
            events: VecDeque::from([
                Input::Message(false),
                Input::Message(true),
                Input::Message(false),
            ]),
            attempts: 0,
            idle_pending: false,
            ordinary_messages: 0,
            closing_seen: false,
        };
        run(&mut owner);
        assert_eq!(owner.attempts, 3);
        assert_eq!(owner.ordinary_messages, 1);
        assert!(owner.closing_seen);
    }
    #[test]
    fn disconnection_drains_instead_of_dropping_owner() {
        let mut owner = Owner {
            native: Rc::new(()),
            events: VecDeque::from([Input::Disconnected]),
            attempts: 0,
            idle_pending: true,
            ordinary_messages: 0,
            closing_seen: false,
        };
        run(&mut owner);
        assert_eq!(owner.attempts, 3);
        assert!(owner.closing_seen);
    }
    #[test]
    fn deferred_resources_retry_while_messages_are_idle() {
        let mut owner = Owner {
            native: Rc::new(()),
            events: VecDeque::from([
                Input::Timeout,
                Input::Timeout,
                Input::Timeout,
                Input::Message(true),
            ]),
            attempts: 0,
            idle_pending: true,
            ordinary_messages: 0,
            closing_seen: false,
        };
        run(&mut owner);
        assert_eq!(owner.attempts, 4);
        assert!(!owner.idle_pending);
        assert!(owner.closing_seen);
    }
}
