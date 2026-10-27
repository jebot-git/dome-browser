/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use glib::prelude::*;
use servo_media_player::PlayerError;

/// Dispatch player messages away from GstPlay's own main thread. GstPlay messages
/// retain their origin: unrefing the last message from a synchronous bus handler
/// can otherwise finalize GstPlay while its stop callback is still on the stack.
pub(crate) struct PlayerSignals(JoinHandle<()>);

impl PlayerSignals {
    pub(crate) fn new(
        player: &gstreamer_play::Play,
    ) -> Result<(gstreamer_play::PlaySignalAdapter, Self), PlayerError> {
        let context = glib::MainContext::new();
        let main_loop = glib::MainLoop::new(Some(&context), false);
        let adapter = gstreamer_play::PlaySignalAdapter::with_main_context(player, &context);
        let thread_adapter = adapter.clone();
        let quit = main_loop.clone();
        // The adapter borrows the player. Keep dispatching queued messages until
        // the player has actually been disposed, including joining its GstPlay
        // thread, then release the adapter and its bus on this dispatch thread.
        player.add_weak_ref_notify(move || quit.quit());

        // A quit before MainLoop::run would be lost. Wait for an actual source
        // dispatch before allowing the caller to release the player.
        let (ready, started) = mpsc::channel();
        let source = glib::idle_source_new(None, glib::Priority::DEFAULT, move || {
            let _ = ready.send(());
            glib::ControlFlow::Break
        });
        source.attach(Some(&context));
        let thread = thread::Builder::new()
            .name("ServoMediaSignals".into())
            .spawn(move || {
                main_loop.run();
                drop(thread_adapter);
            })
            .map_err(|error| PlayerError::Backend(format!("Player signal thread: {error}")))?;
        started.recv().map_err(|error| {
            PlayerError::Backend(format!("Player signal thread startup: {error}"))
        })?;
        Ok((adapter, Self(thread)))
    }

    /// Call after releasing the player, without holding any callback state lock.
    pub(crate) fn join(self) {
        // A client may release a player from its notification callback. That
        // thread will finish the current message and quit on disposal itself.
        if self.0.thread().id() != thread::current().id() {
            let _ = self.0.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;

    #[test]
    fn stop_and_release_while_callback_is_running() {
        gstreamer::init().unwrap();
        for _ in 0..16 {
            let player = gstreamer_play::Play::default();
            let weak = player.downgrade();
            let (adapter, signals) = PlayerSignals::new(&player).unwrap();
            let (entered, receiver) = mpsc::channel();
            let (release, resume) = mpsc::channel();
            let resume = Arc::new(Mutex::new(Some(resume)));
            adapter.connect_error(move |_, _, _| {
                let Some(resume) = resume.lock().unwrap().take() else {
                    return;
                };
                assert_eq!(thread::current().name(), Some("ServoMediaSignals"));
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            player.set_uri(Some("file:///nonexistent-dome-player-lifetime-fixture"));
            player.pause();
            receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            let releaser = thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                release.send(()).unwrap();
            });
            player.stop();
            drop(player);
            drop(adapter);
            signals.join();
            releaser.join().unwrap();
            assert!(
                weak.upgrade().is_none(),
                "player retained after dispatcher joined"
            );
        }
    }

    #[test]
    fn immediate_stop_and_release_drains_dispatcher() {
        gstreamer::init().unwrap();
        for _ in 0..128 {
            let player = gstreamer_play::Play::default();
            let weak = player.downgrade();
            let (adapter, signals) = PlayerSignals::new(&player).unwrap();
            player.stop();
            drop(player);
            drop(adapter);
            signals.join();
            assert!(weak.upgrade().is_none());
        }
    }
}
