/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::{Arc, Mutex, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use servo_base::generic_channel::GenericCallback;
use servo_media_gstreamer::player::GStreamerPlayer;
use servo_media_player::{Player, PlayerEvent, StreamType};
use servo_media_player::context::{GlApi, GlContext, NativeDisplay, PlayerGLContext};
use servo_media_player::video::{VideoFrame, VideoFrameRenderer};
use servo_media_traits::{BackendMsg, ClientContextId};

struct Headless;
impl PlayerGLContext for Headless {
    fn get_gl_context(&self) -> GlContext { GlContext::Unknown }
    fn get_native_display(&self) -> NativeDisplay { NativeDisplay::Headless }
    fn get_gl_api(&self) -> GlApi { GlApi::None }
}
struct Frames(Arc<AtomicUsize>);
impl VideoFrameRenderer for Frames {
    fn render(&mut self, frame: VideoFrame) {
        assert_eq!((frame.get_width(), frame.get_height()), (160, 90));
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
#[test]
fn byte_fed_player_play_and_shutdown() {
    gstreamer::init().unwrap();
    let video = std::fs::read(std::env::var("DOME_MSE_TEST_VIDEO").unwrap()).unwrap();
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = std::thread::spawn(move || {
        while let Ok(BackendMsg::Shutdown { tx_ack, .. }) = shutdown_rx.recv() { let _ = tx_ack.send(()); }
    });
    let backend = Arc::new(Mutex::new(shutdown_tx));
    let frames = Arc::new(AtomicUsize::new(0));
    let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Frames(frames.clone())));
    let (tx, rx) = mpsc::channel();
    let observer = GenericCallback::new(move |event| { let _ = tx.send(event.unwrap()); }).unwrap();
    let player = GStreamerPlayer::new(0, &ClientContextId::build(1, 1), backend.clone(),
        StreamType::Seekable, observer, Some(renderer.clone()), None, Box::new(Headless));
    player.set_input_size(video.len() as u64).unwrap();
    player.set_seekable(false).unwrap();
    player.push_data(video).unwrap();
    player.end_of_stream().unwrap();
    player.play().unwrap();
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            PlayerEvent::Error(error) => panic!("player error: {error}"),
            PlayerEvent::EndOfStream => break,
            _ => (),
        }
    }
    let count = frames.load(Ordering::Relaxed);
    assert!(count >= 30, "frames={count}");
    player.stop().unwrap();
    drop(player);
    drop(renderer);
    drop(backend);
    shutdown_thread.join().unwrap();
    println!("network frames={count} stopped");
    // SAFETY: All objects created by this single-test binary have been released.
    unsafe { gstreamer::deinit(); }
}
