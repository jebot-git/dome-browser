/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![cfg(feature = "native-mse")]

use std::time::Duration;
use gstreamer::prelude::*;
use servo_media_gstreamer::mse::{BufferEvent, Event, Session};

fn open_session() -> (Session, gstreamer::Element) {
    let session = Session::new().unwrap();
    assert!(session.duration().is_nan());
    let player = gstreamer::ElementFactory::make("playbin3").build().unwrap();
    let sink = gstreamer::ElementFactory::make("fakesink").property("sync", false).build().unwrap();
    player.set_property("video-sink", &sink);
    let source = session.native_source().clone();
    player.connect("source-setup", false, move |values| {
        let element = values[1].get::<gstreamer::Element>().unwrap();
        source.attach(element.downcast_ref::<gstreamer_mse::MseSrc>().unwrap());
        None
    });
    player.set_property("uri", "mse://rust-session-regression");
    player.set_state(gstreamer::State::Paused).unwrap();
    assert_eq!(session.events().recv_timeout(Duration::from_secs(8)).unwrap(), Event::SourceOpen);
    assert!(session.duration().is_nan());
    (session, player)
}

fn append_events(session: &Session) -> Vec<BufferEvent> {
    let mut result = Vec::new();
    loop {
        if let Event::Buffer(_, event) = session.events().recv_timeout(Duration::from_secs(8)).unwrap() {
            result.push(event);
            if event == BufferEvent::UpdateEnd { return result; }
        }
    }
}

#[test]
fn native_session_contract_and_shutdown() {
    // Run this integration binary under a process watchdog: gst_deinit waits
    // for every native task, including tasks retained by leaked references.
    let video = std::fs::read(std::env::var("DOME_MSE_TEST_VIDEO")
        .expect("Set DOME_MSE_TEST_VIDEO to the synthetic 2-second baseline MP4 fixture")).unwrap();
    let mime = "video/mp4; codecs=\"avc1.42E01E\"";
    {
        let (mut session, player) = open_session();
        for invalid in [f64::NAN, f64::NEG_INFINITY, -1., f64::MAX] {
            assert!(session.set_duration(invalid).is_err());
        }
        session.set_duration(f64::INFINITY).unwrap();
        assert_eq!(session.duration(), f64::INFINITY);
        session.set_duration(30.).unwrap();
        let id = session.add_buffer(mime).unwrap();
        assert!(session.add_buffer("video/mp4\0bad").is_err());
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            assert!(session.set_timestamp_offset(id, invalid).is_err());
        }
        session.set_timestamp_offset(id, -0.5).unwrap();
        assert_eq!(session.timestamp_offset(id).unwrap(), -0.5);
        session.append(id, video.clone()).unwrap();
        assert_eq!(append_events(&session), [BufferEvent::UpdateStart, BufferEvent::Update, BufferEvent::UpdateEnd]);
        assert_eq!(session.duration(), 30.);
        let ranges = session.buffered(id).unwrap();
        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].start - 0.5).abs() < 0.00001, "{ranges:?}");
        assert!((ranges[0].end - 1.5).abs() < 0.00001, "{ranges:?}");
        session.remove_range(id, 0. .. f64::INFINITY).unwrap();
        assert_eq!(append_events(&session), [BufferEvent::UpdateStart, BufferEvent::Update, BufferEvent::UpdateEnd]);
        assert!(session.buffered(id).unwrap().is_empty());
        session.remove_buffer(id).unwrap();
        assert!(session.append(id, video.clone()).is_err());
        assert!(session.remove_buffer(id).is_err());
        assert!(session.buffered(id).is_err());
        player.set_state(gstreamer::State::Null).unwrap();
        drop(session);
        drop(player);
    }
    {
        let (mut session, player) = open_session();
        let id = session.add_buffer(mime).unwrap();
        let invalid = [0,0,0,24,b'f',b't',b'y',b'p',b'i',b's',b'o',b'5',0,0,2,0,
            b'i',b's',b'o',b'5',b'i',b's',b'o',b'6',0,0,0,4,b'm',b'o',b'o',b'v'];
        session.append(id, invalid.repeat(512)).unwrap();
        assert_eq!(append_events(&session), [BufferEvent::UpdateStart, BufferEvent::Error, BufferEvent::UpdateEnd]);
        assert!(!session.updating(id).unwrap());
        assert!(session.append(id, video.clone()).is_err());
        player.set_state(gstreamer::State::Null).unwrap();
        drop(session);
        drop(player);
    }
    {
        let (mut session, player) = open_session();
        let id = session.add_buffer(mime).unwrap();
        session.append(id, video.clone()).unwrap();
        assert!(session.updating(id).unwrap());
        session.remove_buffer(id).unwrap();
        assert_eq!(append_events(&session), [BufferEvent::UpdateStart, BufferEvent::Abort, BufferEvent::UpdateEnd]);
        player.set_state(gstreamer::State::Null).unwrap();
        drop(session);
        drop(player);
    }
    {
        let (mut session, player) = open_session();
        let id = session.add_buffer(mime).unwrap();
        session.append(id, video).unwrap();
        assert_eq!(append_events(&session), [BufferEvent::UpdateStart, BufferEvent::Update, BufferEvent::UpdateEnd]);
        // No EOS and no explicit buffer removal. Session Drop must join feeds
        // that would otherwise retain this unfinished stream indefinitely.
        player.set_state(gstreamer::State::Null).unwrap();
        drop(session);
        drop(player);
    }
    {
        let (mut session, player) = open_session();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = std::sync::Mutex::new(release_rx);
        session.native_source().connect_on_source_close(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(8)).unwrap();
        });
        player.set_state(gstreamer::State::Null).unwrap();
        session.detach();
        entered_rx.recv_timeout(Duration::from_secs(8)).unwrap();
        // Force the signal emission to hold the final owner reference. Its
        // release must not dispose/join the event queue on that same task.
        drop(session);
        drop(player);
        release_tx.send(()).unwrap();
    }
    // SAFETY: All objects and callbacks created by this integration test have
    // been released, and this test binary contains no other initialized tests.
    unsafe { gstreamer::deinit(); }
}
