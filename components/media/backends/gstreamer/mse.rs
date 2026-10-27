/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Experimental MSE session ownership and native event transport.
//!
//! Native callbacks only enqueue events. The owner must process them on its
//! media/script event loop; callbacks must never run DOM work or teardown.

use std::ops::Range;
use std::sync::mpsc::{self, Receiver, Sender};

use glib::translate::*;
use gstreamer::prelude::*;
use gstreamer_mse::{MediaSource, MediaSourceError, SourceBuffer};
pub use gstreamer_mse::{MediaSourceEOSError, MediaSourceReadyState, SourceBufferAppendMode};

pub const PATCH_SHA256: &str = env!("DOME_MSE_PATCH_SHA256");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferEvent { UpdateStart, Update, UpdateEnd, Error, Abort }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    SourceOpen,
    SourceEnded,
    SourceClosed,
    Buffer(BufferId, BufferEvent),
}

/// One owner for source/buffer lifetime. Drop this after stopping the player.
/// This is intentionally not Clone: cloned native objects alone cannot perform
/// the removal needed to release track tasks waiting for further appends.
pub struct Session {
    source: MediaSource,
    buffers: Vec<Option<SourceBuffer>>,
    sender: Sender<Event>,
    events: Receiver<Event>,
}

fn type_error(message: &str) -> glib::Error {
    glib::Error::new(MediaSourceError::Type, message)
}

fn finite_time(seconds: f64) -> Result<gstreamer::ClockTime, glib::Error> {
    let nanos = seconds * 1_000_000_000.;
    if !seconds.is_finite() || seconds < 0. || nanos >= u64::MAX as f64 {
        return Err(type_error("time is outside the native clock range"));
    }
    Ok(gstreamer::ClockTime::from_nseconds(nanos.round() as u64))
}

impl Session {
    pub fn new() -> Result<Self, glib::Error> {
        gstreamer::init()?;
        let source = MediaSource::new();
        if source.find_property("duration-initialized").is_none() {
            return Err(glib::Error::new(MediaSourceError::NotSupported,
                "native-mse loaded an incompatible library at runtime"));
        }
        let (sender, events) = mpsc::channel();
        let tx = sender.clone();
        source.connect_on_source_open(move |_| { let _ = tx.send(Event::SourceOpen); });
        let tx = sender.clone();
        source.connect_on_source_ended(move |_| { let _ = tx.send(Event::SourceEnded); });
        let tx = sender.clone();
        source.connect_on_source_close(move |_| { let _ = tx.send(Event::SourceClosed); });
        Ok(Self { source, buffers: Vec::new(), sender, events })
    }

    /// Used by GstPlay's source-setup callback. It may retain a clone until
    /// source setup completes, but the Session remains the lifetime owner.
    pub fn native_source(&self) -> &MediaSource { &self.source }
    pub fn events(&self) -> &Receiver<Event> { &self.events }
    pub fn ready_state(&self) -> MediaSourceReadyState { self.source.ready_state() }

    pub fn add_buffer(&mut self, mime: &str) -> Result<BufferId, glib::Error> {
        if mime.contains('\0') { return Err(type_error("MIME type contains NUL")); }
        let buffer = self.source.add_source_buffer(mime)?;
        let id = BufferId(self.buffers.len());
        for (signal, event) in [
            ("on-update-start", BufferEvent::UpdateStart),
            ("on-update", BufferEvent::Update),
            ("on-update-end", BufferEvent::UpdateEnd),
            ("on-error", BufferEvent::Error),
            ("on-abort", BufferEvent::Abort),
        ] {
            let sender = self.sender.clone();
            buffer.connect(signal, false, move |_| {
                let _ = sender.send(Event::Buffer(id, event));
                None
            });
        }
        self.buffers.push(Some(buffer));
        Ok(id)
    }

    fn buffer(&self, id: BufferId) -> Result<&SourceBuffer, glib::Error> {
        self.buffers.get(id.0).and_then(Option::as_ref).ok_or_else(||
            glib::Error::new(MediaSourceError::InvalidState, "buffer was removed"))
    }

    pub fn remove_buffer(&mut self, id: BufferId) -> Result<(), glib::Error> {
        self.source.remove_source_buffer(self.buffer(id)?)?;
        self.buffers[id.0] = None;
        Ok(())
    }

    pub fn append(&self, id: BufferId, bytes: Vec<u8>) -> Result<(), glib::Error> {
        self.buffer(id)?.append_buffer(gstreamer::Buffer::from_mut_slice(bytes))
    }

    pub fn updating(&self, id: BufferId) -> Result<bool, glib::Error> {
        Ok(self.buffer(id)?.is_updating())
    }

    pub fn abort(&self, id: BufferId) -> Result<(), glib::Error> { self.buffer(id)?.abort() }

    pub fn set_mode(&self, id: BufferId, mode: SourceBufferAppendMode) -> Result<(), glib::Error> {
        self.buffer(id)?.set_append_mode(mode)
    }

    pub fn set_duration(&self, seconds: f64) -> Result<(), glib::Error> {
        let duration = if seconds == f64::INFINITY { None } else { Some(finite_time(seconds)?) };
        self.source.set_duration(duration)
    }

    /// Native CLOCK_TIME_NONE represents both unknown and infinity. The pinned
    /// backend's initialized flag preserves the web API distinction.
    pub fn duration(&self) -> f64 {
        if self.ready_state() == MediaSourceReadyState::Closed ||
            !self.source.property::<bool>("duration-initialized") {
            f64::NAN
        } else {
            self.source.duration().map_or(f64::INFINITY, |t| t.seconds_f64())
        }
    }

    pub fn set_timestamp_offset(&self, id: BufferId, seconds: f64) -> Result<(), glib::Error> {
        let buffer = self.buffer(id)?;
        let nanos = seconds * 1_000_000_000.;
        if !seconds.is_finite() || nanos < i64::MIN as f64 || nanos >= i64::MAX as f64 {
            return Err(type_error("timestamp offset is outside the signed clock range"));
        }
        // The public C ABI uses guint64, but the pinned backend interprets the
        // bit pattern as gint64. ClockTime cannot represent negative offsets.
        unsafe {
            let mut error = std::ptr::null_mut();
            let success = gstreamer_mse::ffi::gst_source_buffer_set_timestamp_offset(
                buffer.to_glib_none().0, (nanos.round() as i64) as u64, &mut error);
            if success == glib::ffi::GFALSE { Err(from_glib_full(error)) } else { Ok(()) }
        }
    }

    pub fn timestamp_offset(&self, id: BufferId) -> Result<f64, glib::Error> {
        Ok(self.buffer(id)?.property::<i64>("timestamp-offset") as f64 / 1_000_000_000.)
    }

    pub fn remove_range(&self, id: BufferId, range: Range<f64>) -> Result<(), glib::Error> {
        let end = if range.end == f64::INFINITY { None } else { Some(finite_time(range.end)?) };
        self.buffer(id)?.remove(finite_time(range.start)?, end)
    }

    pub fn buffered(&self, id: BufferId) -> Result<Vec<Range<f64>>, glib::Error> {
        let buffer = self.buffer(id)?;
        buffered_ranges(buffer)
    }

    pub fn detach(&mut self) {
        for buffer in self.buffers.iter_mut().filter_map(Option::take) {
            let _ = self.source.remove_source_buffer(&buffer);
        }
        self.source.detach();
    }

    pub fn end_of_stream(&self, error: MediaSourceEOSError) -> Result<(), glib::Error> {
        self.source.end_of_stream(error)
    }
}

impl Drop for Session {
    fn drop(&mut self) { self.detach(); }
}

fn buffered_ranges(buffer: &SourceBuffer) -> Result<Vec<Range<f64>>, glib::Error> {
    // The upstream bindings omit GArray<GstMediaSourceRange>. The returned
    // array owns its data, remains live throughout copying, and is unreffed
    // exactly once. Its public C element layout is pinned with the library.
    unsafe {
        let mut error = std::ptr::null_mut();
        let array = gstreamer_mse::ffi::gst_source_buffer_get_buffered(buffer.to_glib_none().0, &mut error);
        if !error.is_null() { return Err(from_glib_full(error)); }
        assert!(!array.is_null());
        let mut ranges = Vec::with_capacity((*array).len as usize);
        for index in 0..(*array).len as usize {
            let range = &*((*array).data.cast::<gstreamer_mse::ffi::GstMediaSourceRange>().add(index));
            ranges.push(range.start as f64 / 1_000_000_000. .. range.end as f64 / 1_000_000_000.);
        }
        glib::ffi::g_array_unref(array);
        Ok(ranges)
    }
}

/// Shared ownership for the browser player and its DOM/media session owner.
/// Native source attachment never takes the session mutex, so the owner can
/// wait for source-open without blocking the GstPlay source-setup callback.
pub struct SharedSession {
    session: std::sync::Mutex<Session>,
    source: MediaSource,
}

impl SharedSession {
    pub fn new() -> Result<Self, glib::Error> {
        let session = Session::new()?;
        let source = session.native_source().clone();
        Ok(Self { session: std::sync::Mutex::new(session), source })
    }

    pub fn with_session<R>(&self, callback: impl FnOnce(&mut Session) -> R) -> R {
        callback(&mut self.session.lock().unwrap())
    }

    pub(crate) fn attach(&self, element: &gstreamer_mse::MseSrc) {
        self.source.attach(element);
    }
}

impl servo_media_player::MediaSourceInput for SharedSession {
    fn as_any(&self) -> &dyn std::any::Any { self }

    fn buffered(&self) -> Vec<Range<f64>> {
        let active = self.source.active_source_buffers();
        let mut groups = Vec::new();
        let mut highest_end: f64 = 0.;
        for i in 0..active.length() {
            let Some(buffer) = active.index(i) else { continue; };
            let ranges = buffered_ranges(&buffer).unwrap_or_default();
            if let Some(last) = ranges.last() { highest_end = highest_end.max(last.end); }
            groups.push(ranges);
        }
        if groups.is_empty() || highest_end == 0. { return Vec::new(); }
        let mut intersection = vec![0. .. highest_end];
        for mut ranges in groups {
            if self.source.ready_state() == MediaSourceReadyState::Ended {
                if let Some(last) = ranges.last_mut() { last.end = highest_end; }
            }
            let mut next = Vec::new();
            for a in &intersection {
                for b in &ranges {
                    let start = a.start.max(b.start);
                    let end = a.end.min(b.end);
                    if start < end { next.push(start..end); }
                }
            }
            intersection = next;
        }
        intersection
    }

    fn seekable(&self) -> Vec<Range<f64>> {
        let duration = self.with_session(|session| session.duration());
        if duration.is_nan() || duration <= 0. { return Vec::new(); }
        if duration.is_finite() { return vec![0. .. duration]; }
        let ranges = self.buffered();
        let live = self.source.live_seekable_range();
        let mut end = ranges.last().map_or(0., |r| r.end);
        if live.end() > live.start() {
            let start = ranges.first().map_or(live.start().seconds_f64(), |r|
                r.start.min(live.start().seconds_f64()));
            end = end.max(live.end().seconds_f64());
            return vec![start..end];
        }
        if end > 0. { vec![0. .. end] } else { Vec::new() }
    }

    fn detach(&self) { self.with_session(Session::detach); }
}
