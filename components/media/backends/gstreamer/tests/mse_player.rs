/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
#![cfg(feature = "native-mse")]

use std::sync::{Arc, Mutex, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use servo_base::generic_channel::GenericCallback;
use servo_media_gstreamer::mse::{BufferEvent, Event, MediaSourceEOSError, MediaSourceReadyState, SharedSession};
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
        assert!(!frame.is_gl_texture());
        assert_eq!(frame.get_data().len(), 160 * 90 * 4);
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn servo_player_mse_play_seek_and_stop() {
    let video = std::fs::read(std::env::var("DOME_MSE_TEST_VIDEO").unwrap()).unwrap();
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = std::thread::spawn(move || {
        while let Ok(BackendMsg::Shutdown { tx_ack, .. }) = shutdown_rx.recv() { let _ = tx_ack.send(()); }
    });
    let backend = Arc::new(Mutex::new(shutdown_tx));
    for (cycle, seek) in [None, Some(1.), None].into_iter().enumerate() {
        let source = Arc::new(SharedSession::new().unwrap());
        let frames = Arc::new(AtomicUsize::new(0));
        let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Frames(frames.clone())));
        let (tx, rx) = mpsc::channel();
        let observer = GenericCallback::new(move |event| { let _ = tx.send(event.unwrap()); }).unwrap();
        let player = GStreamerPlayer::new(cycle, &ClientContextId::build(1, 1), backend.clone(),
            StreamType::Seekable, observer, Some(renderer.clone()), None, Box::new(Headless));
        player.set_media_source(source.clone()).unwrap();
        let start = Instant::now();
        player.pause().unwrap();
        assert!(start.elapsed() < Duration::from_secs(8), "setup waited for unappended bytes");
        source.with_session(|session| {
            assert_eq!(session.events().recv_timeout(Duration::from_secs(8)).unwrap(), Event::SourceOpen);
            let id = session.add_buffer("video/mp4; codecs=\"avc1.42E01E\"").unwrap();
            session.append(id, video.clone()).unwrap();
            let mut events = Vec::new();
            loop {
                if let Event::Buffer(_, event) = session.events().recv_timeout(Duration::from_secs(8)).unwrap() {
                    events.push(event);
                    if event == BufferEvent::UpdateEnd { break; }
                }
            }
            assert_eq!(events, [BufferEvent::UpdateStart, BufferEvent::Update, BufferEvent::UpdateEnd]);
            if cycle != 2 { session.end_of_stream(MediaSourceEOSError::None).unwrap(); }
        });
        let ranges = player.buffered();
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert!((ranges[0].start - 0.).abs() < 0.00001 && (ranges[0].end - 2.).abs() < 0.00001);
        assert_eq!(player.seekable(), vec![0. .. 2.]);
        for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, f64::MAX] {
            assert!(player.seek(time).is_err());
        }
        if let Some(time) = seek { player.seek(time).unwrap(); }
        player.play().unwrap();
        let mut seek_done = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                PlayerEvent::Error(error) => panic!("player error: {error}"),
                PlayerEvent::NeedData | PlayerEvent::EnoughData | PlayerEvent::SeekData(..) => panic!("MSE used network data callbacks"),
                PlayerEvent::SeekDone(time) => { assert!((time - seek.unwrap()).abs() < 0.01); seek_done = true; }
                PlayerEvent::EndOfStream => break,
                _ => (),
            }
            if cycle == 2 && frames.load(Ordering::Relaxed) >= 2 { break; }
        }
        assert!(seek.is_none() || seek_done);
        let count = frames.load(Ordering::Relaxed);
        assert!(count >= if cycle == 2 { 2 } else if seek.is_some() { 15 } else { 30 }, "frames={count}");
        assert!(player.set_media_source(source.clone()).is_err());
        player.stop().unwrap();
        assert!(player.buffered().is_empty());
        assert!(player.seekable().is_empty());
        source.with_session(|session| assert_eq!(session.ready_state(), MediaSourceReadyState::Closed));
        drop(player);
        drop(renderer);
        drop(source);
        println!("cycle={cycle} seek={seek:?} frames={count} stopped");
    }
    drop(backend);
    shutdown_thread.join().unwrap();
    // SAFETY: All test players, renderers, source sessions and callbacks have
    // been dropped. A watchdog around this process detects retained task leaks.
    unsafe { gstreamer::deinit(); }
}
