/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

pub mod audio_decoder;
pub mod audio_sink;
pub mod audio_stream_reader;
mod datachannel;
mod device_monitor;
pub mod media_capture;
pub mod media_stream;
mod media_stream_source;
pub mod player;
mod player_signals;
#[cfg(feature = "native-mse")]
pub mod mse;
mod registry_scanner;
mod render;
mod source;
pub mod webrtc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};
use std::thread;
use std::vec::Vec;

use data_url::mime::Mime;
use device_monitor::GStreamerDeviceMonitor;
use gstreamer::prelude::*;
use log::warn;
use media_stream::GStreamerMediaStream;
use registry_scanner::GSTREAMER_REGISTRY_SCANNER;
use servo_base::generic_channel::GenericCallback;
use servo_media::{Backend, BackendDeInit, BackendInit, MediaInstanceError, SupportsMediaType};
use servo_media_audio::context::{AudioContext, AudioContextOptions};
use servo_media_audio::decoder::AudioDecoder;
use servo_media_audio::sink::AudioSinkError;
use servo_media_audio::{AudioBackend, AudioStreamReader};
use servo_media_player::audio::AudioRenderer;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{Player, PlayerEvent, StreamType};
use servo_media_streams::capture::MediaTrackConstraintSet;
use servo_media_streams::device_monitor::MediaDeviceMonitor;
use servo_media_streams::registry::MediaStreamId;
use servo_media_streams::{MediaOutput, MediaSocket, MediaStreamType};
use servo_media_traits::{BackendMsg, ClientContextId, MediaInstance};
use servo_media_webrtc::{WebRtcBackend, WebRtcController, WebRtcSignaller};

static BACKEND_BASE_TIME: LazyLock<gstreamer::ClockTime> =
    LazyLock::new(|| gstreamer::SystemClock::obtain().time());

static BACKEND_THREAD: OnceLock<bool> = OnceLock::new();

pub type WeakMediaInstance = Weak<Mutex<dyn MediaInstance>>;
pub type WeakMediaInstanceHashMap = HashMap<ClientContextId, Vec<(usize, WeakMediaInstance)>>;

pub struct GStreamerBackend {
    capture_mocking: AtomicBool,
    instances: Arc<Mutex<WeakMediaInstanceHashMap>>,
    next_instance_id: AtomicUsize,
    /// Channel to communicate media instances with its owner Backend.
    backend_chan: Arc<Mutex<Sender<BackendMsg>>>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ErrorLoadingPlugins<'a>(Vec<&'a str>);

fn start_shutdown_thread(
    instances: Arc<Mutex<WeakMediaInstanceHashMap>>,
    receiver: mpsc::Receiver<BackendMsg>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("GStreamerBackend ShutdownThread".to_owned())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    BackendMsg::Shutdown { context, id, tx_ack } => {
                        {
                            let mut instances = instances.lock().unwrap();
                            if let Some(entries) = instances.get_mut(&context) {
                                entries.retain(|entry| entry.0 != id);
                                if entries.is_empty() {
                                    instances.remove(&context);
                                }
                            }
                        }
                        // The registry lock is released before the owner resumes.
                        // A caller may have gone away while its record was queued.
                        let _ = tx_ack.send(());
                    },
                }
            }
        })
        .unwrap()
}

impl GStreamerBackend {
    pub fn init_with_plugins<'a, T: AsRef<str>>(
        plugin_dir: PathBuf,
        plugins: &'a [T],
    ) -> Result<Box<dyn Backend>, ErrorLoadingPlugins<'a>> {
        gstreamer::init().unwrap();

        // GStreamer between 1.19.1 and 1.22.7 will not send messages like "end of stream"
        // to GstPlayer unless there is a GLib main loop running somewhere. We should remove
        // this workaround when we raise of required version of GStreamer.
        // See https://github.com/servo/media/pull/393.
        let needs_background_glib_main_loop = {
            let (major, minor, micro, _) = gstreamer::version();
            (major, minor, micro) >= (1, 19, 1) && (major, minor, micro) <= (1, 22, 7)
        };

        if needs_background_glib_main_loop {
            BACKEND_THREAD.get_or_init(|| {
                thread::spawn(|| glib::MainLoop::new(None, false).run());
                true
            });
        }

        #[cfg(target_os = "linux")]
        {
            // Remove the va plugins that have some issues on certain platforms.
            // When the driver is fixed we will remove this.
            let registry = gstreamer::Registry::get();
            if let Some(plugin) = registry.find_plugin("va") {
                registry.remove_plugin(&plugin);
            }
            if let Some(plugin) = registry.find_plugin("vaapi") {
                registry.remove_plugin(&plugin);
            }
        }

        // This is a workaround for a race condition in GStreamer. Real fix is
        // in
        // https://gitlab.freedesktop.org/gstreamer/gstreamer/-/merge_requests/12194
        // . This will hold a connection to the server but it won't create
        // streams. This happens during READY -> PAUSED.
        #[cfg(target_os = "linux")]
        {
            static PULSESINK_KEEPALIVE: std::sync::OnceLock<Option<gstreamer::Element>> =
                std::sync::OnceLock::new();
            PULSESINK_KEEPALIVE.get_or_init(|| {
                let sink = gstreamer::ElementFactory::make("pulsesink").build().ok()?;
                sink.set_state(gstreamer::State::Ready).ok()?;
                Some(sink)
            });
        }

        let mut errors = vec![];
        for plugin in plugins {
            let mut path = plugin_dir.clone();
            path.push(plugin.as_ref());
            let registry = gstreamer::Registry::get();
            if gstreamer::Plugin::load_file(&path)
                .is_ok_and(|plugin| registry.add_plugin(&plugin).is_ok())
            {
                continue;
            }
            errors.push(plugin.as_ref());
        }

        if !errors.is_empty() {
            return Err(ErrorLoadingPlugins(errors));
        }

        type MediaInstancesVec = Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>;
        let instances: HashMap<ClientContextId, MediaInstancesVec> = Default::default();
        let instances = Arc::new(Mutex::new(instances));

        let (backend_chan, recvr) = mpsc::channel();
        start_shutdown_thread(instances.clone(), recvr);

        Ok(Box::new(GStreamerBackend {
            capture_mocking: AtomicBool::new(false),
            instances,
            next_instance_id: AtomicUsize::new(0),
            backend_chan: Arc::new(Mutex::new(backend_chan)),
        }))
    }

    fn media_instance_action(
        &self,
        id: &ClientContextId,
        cb: &dyn Fn(&dyn MediaInstance) -> Result<(), MediaInstanceError>,
    ) {
        // Keep upgraded instances alive until after releasing the registry.
        // Their last Arc can be dropped by this action: destruction waits for
        // ShutdownThread to remove the same instance from this registry.
        let active = {
            let mut instances = self.instances.lock().unwrap();
            let Some(entries) = instances.get_mut(id) else {
                warn!("Trying to exec media action on an unknown client context");
                return;
            };
            let mut active = Vec::with_capacity(entries.len());
            entries.retain(|(_, weak)| match weak.upgrade() {
                Some(instance) => {
                    active.push(instance);
                    true
                },
                None => false,
            });
            if entries.is_empty() {
                instances.remove(id);
            }
            active
        };
        for instance in active {
            if cb(&*instance.lock().unwrap()).is_err() {
                warn!("Error executing media instance action");
            }
        }
    }
}

impl Backend for GStreamerBackend {
    #[expect(clippy::redundant_clone, reason = "False positive")]
    fn create_player(
        &self,
        context_id: &ClientContextId,
        stream_type: StreamType,
        sender: GenericCallback<PlayerEvent>,
        renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        let id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let player = Arc::new(Mutex::new(player::GStreamerPlayer::new(
            id,
            context_id,
            self.backend_chan.clone(),
            stream_type,
            sender,
            renderer,
            audio_renderer,
            gl_context,
        )));
        let mut instances = self.instances.lock().unwrap();
        let entry = instances.entry(*context_id).or_default();
        entry.push((id, Arc::downgrade(&player).clone()));
        player
    }

    #[expect(clippy::redundant_clone, reason = "False positive")]
    fn create_audio_context(
        &self,
        client_context_id: &ClientContextId,
        options: AudioContextOptions,
    ) -> Result<Arc<Mutex<AudioContext>>, AudioSinkError> {
        let id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let audio_context =
            AudioContext::new::<Self>(id, client_context_id, self.backend_chan.clone(), options)?;

        let audio_context = Arc::new(Mutex::new(audio_context));

        let mut instances = self.instances.lock().unwrap();
        let entry = instances.entry(*client_context_id).or_default();
        entry.push((id, Arc::downgrade(&audio_context).clone()));

        Ok(audio_context)
    }

    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController {
        WebRtcController::new::<Self>(signaller)
    }

    fn create_audiostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_audio()
    }

    fn create_videostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_video()
    }

    fn create_stream_output(&self) -> Box<dyn MediaOutput> {
        Box::new(media_stream::MediaSink::default())
    }

    fn create_stream_and_socket(
        &self,
        ty: MediaStreamType,
    ) -> (Box<dyn MediaSocket>, MediaStreamId) {
        let (id, socket) = GStreamerMediaStream::create_proxy(ty);
        (Box::new(socket), id)
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        if self.capture_mocking.load(Ordering::Acquire) {
            // XXXManishearth we should caps filter this
            return Some(self.create_audiostream());
        }
        media_capture::create_audioinput_stream(set)
    }

    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        if self.capture_mocking.load(Ordering::Acquire) {
            // XXXManishearth we should caps filter this
            return Some(self.create_videostream());
        }
        media_capture::create_videoinput_stream(set)
    }

    fn can_play_type(&self, media_type: &str) -> SupportsMediaType {
        if let Ok(mime) = media_type.parse::<Mime>() {
            let mime_type = format!("{}/{}", mime.type_, mime.subtype);
            let codecs = match mime.get_parameter("codecs") {
                Some(codecs) => codecs.split(',').map(|codec| codec.trim()).collect(),
                None => vec![],
            };

            if GSTREAMER_REGISTRY_SCANNER.is_container_type_supported(&mime_type) {
                if codecs.is_empty() {
                    return SupportsMediaType::Maybe;
                } else if GSTREAMER_REGISTRY_SCANNER.are_all_codecs_supported(&codecs) {
                    return SupportsMediaType::Probably;
                } else {
                    return SupportsMediaType::No;
                }
            }
        }
        SupportsMediaType::No
    }

    fn set_capture_mocking(&self, mock: bool) {
        self.capture_mocking.store(mock, Ordering::Release)
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.media_instance_action(
            id,
            &(move |instance: &dyn MediaInstance| instance.mute(val)),
        );
    }

    fn suspend(&self, id: &ClientContextId) {
        self.media_instance_action(id, &|instance: &dyn MediaInstance| instance.suspend());
    }

    fn resume(&self, id: &ClientContextId) {
        self.media_instance_action(id, &|instance: &dyn MediaInstance| instance.resume());
    }

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor> {
        Box::new(GStreamerDeviceMonitor::new())
    }
}

impl AudioBackend for GStreamerBackend {
    type Sink = audio_sink::GStreamerAudioSink;
    fn make_decoder() -> Box<dyn AudioDecoder> {
        Box::new(audio_decoder::GStreamerAudioDecoder::new())
    }
    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        audio_sink::GStreamerAudioSink::new()
    }

    fn make_streamreader(
        id: MediaStreamId,
        sample_rate: f32,
    ) -> Result<Box<dyn AudioStreamReader + Send>, AudioSinkError> {
        let reader = audio_stream_reader::GStreamerAudioStreamReader::new(id, sample_rate)
            .map_err(AudioSinkError::Backend)?;
        Ok(Box::new(reader))
    }
}

impl WebRtcBackend for GStreamerBackend {
    type Controller = webrtc::GStreamerWebRtcController;

    fn construct_webrtc_controller(
        signaller: Box<dyn WebRtcSignaller>,
        thread: WebRtcController,
    ) -> Self::Controller {
        webrtc::construct(signaller, thread).expect("WebRTC creation failed")
    }
}

impl BackendInit for GStreamerBackend {
    fn init() -> Box<dyn Backend> {
        Self::init_with_plugins::<&str>(PathBuf::new(), &[]).unwrap()
    }
}

impl BackendDeInit for GStreamerBackend {
    fn deinit(&self) {
        let to_shutdown: Vec<(ClientContextId, usize)> = {
            let map = self.instances.lock().unwrap();
            map.iter()
                .flat_map(|(ctx, v)| v.iter().map(move |(id, _)| (*ctx, *id)))
                .collect()
        };

        for (ctx, id) in to_shutdown {
            let (tx_ack, rx_ack) = mpsc::channel();
            let _ = self
                .backend_chan
                .lock()
                .unwrap()
                .send(BackendMsg::Shutdown {
                    context: ctx,
                    id,
                    tx_ack,
                });
            let _ = rx_ack.recv();
        }
    }
}

#[cfg(test)]
mod registry_lifetime_tests {
    use std::time::Duration;

    use super::*;

    struct Instance {
        id: usize,
        cleanup: Option<(Sender<BackendMsg>, Sender<bool>)>,
    }

    impl MediaInstance for Instance {
        fn get_id(&self) -> usize { self.id }
        fn mute(&self, _: bool) -> Result<(), MediaInstanceError> { Ok(()) }
        fn suspend(&self) -> Result<(), MediaInstanceError> { Ok(()) }
        fn resume(&self) -> Result<(), MediaInstanceError> { Ok(()) }
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            if let Some((sender, result)) = &self.cleanup {
                let (tx_ack, rx_ack) = mpsc::channel();
                let sent = sender.send(BackendMsg::Shutdown {
                    context: ClientContextId::build(1, 1), id: self.id, tx_ack,
                }).is_ok();
                // Bound the baseline deadlock so the regression fails without
                // leaving blocked test threads or an unbounded test process.
                let acknowledged = sent && rx_ack.recv_timeout(Duration::from_millis(500)).is_ok();
                let _ = result.send(acknowledged);
            }
        }
    }

    fn instance(id: usize) -> Arc<Mutex<dyn MediaInstance>> {
        Arc::new(Mutex::new(Instance { id, cleanup: None }))
    }

    #[test]
    fn shutdown_consumer_drains_multiple_contexts_and_tolerates_lost_ack() {
        let a = ClientContextId::build(1, 1);
        let b = ClientContextId::build(2, 1);
        let one = instance(1); let two = instance(2); let three = instance(3);
        let instances = Arc::new(Mutex::new(HashMap::from([
            (a, vec![(1, Arc::downgrade(&one)), (2, Arc::downgrade(&two))]),
            (b, vec![(3, Arc::downgrade(&three))]),
        ])));
        let (sender, receiver) = mpsc::channel();
        let worker = start_shutdown_thread(instances.clone(), receiver);
        let mut acknowledgements = 0;
        for (context, id) in [(a, 1), (b, 99), (a, 1), (a, 2), (b, 3)] {
            let (tx_ack, rx_ack) = mpsc::channel();
            if sender.send(BackendMsg::Shutdown { context, id, tx_ack }).is_ok() &&
                rx_ack.recv_timeout(Duration::from_secs(2)).is_ok() {
                acknowledgements += 1;
            }
        }
        let (tx_ack, rx_ack) = mpsc::channel();
        drop(rx_ack);
        let _ = sender.send(BackendMsg::Shutdown { context: a, id: 1, tx_ack });
        let (tx_ack, rx_ack) = mpsc::channel();
        let sent_after_lost_ack = sender.send(BackendMsg::Shutdown { context: b, id: 3, tx_ack }).is_ok();
        let continued_after_lost_ack = sent_after_lost_ack && rx_ack.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(sender);
        let joined = worker.join().is_ok();
        assert_eq!(acknowledgements, 5, "later instance closures must be consumed");
        assert!(continued_after_lost_ack && joined && instances.lock().unwrap().is_empty());
    }

    #[test]
    fn shutdown_consumer_exits_cleanly_without_any_instances() {
        let (sender, receiver) = mpsc::channel();
        let worker = start_shutdown_thread(Arc::new(Mutex::new(HashMap::new())), receiver);
        drop(sender);
        assert!(worker.join().is_ok());
    }

    #[test]
    fn backend_deinit_and_late_player_drops_clear_all_contexts() {
        use servo_media_player::context::{GlApi, GlContext, NativeDisplay};
        struct Headless;
        impl PlayerGLContext for Headless {
            fn get_gl_context(&self) -> GlContext { GlContext::Unknown }
            fn get_native_display(&self) -> NativeDisplay { NativeDisplay::Headless }
            fn get_gl_api(&self) -> GlApi { GlApi::None }
        }
        gstreamer::init().unwrap();
        let instances = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::channel();
        let worker = start_shutdown_thread(instances.clone(), receiver);
        let backend = GStreamerBackend { capture_mocking: AtomicBool::new(false),
            instances: instances.clone(), next_instance_id: AtomicUsize::new(0),
            backend_chan: Arc::new(Mutex::new(sender)), };
        let mut players = Vec::new();
        for n in 0..16 {
            players.push(backend.create_player(&ClientContextId::build(n / 4, n % 4 + 1),
                StreamType::Seekable, GenericCallback::new(|_| {}).unwrap(),
                None, None, Box::new(Headless)));
        }
        assert_eq!(instances.lock().unwrap().len(), 16);
        backend.deinit();
        assert!(instances.lock().unwrap().is_empty());
        drop(players);
        assert!(instances.lock().unwrap().is_empty());
        drop(backend);
        worker.join().unwrap();
    }

    #[test]
    fn last_owner_during_action_can_complete_cleanup_and_preserve_new_instances() {
        let context = ClientContextId::build(1, 1);
        let (sender, receiver) = mpsc::channel();
        let (cleanup_result, result) = mpsc::channel();
        let one: Arc<Mutex<dyn MediaInstance>> = Arc::new(Mutex::new(Instance {
            id: 1, cleanup: Some((sender.clone(), cleanup_result)),
        }));
        let two = instance(2);
        let instances = Arc::new(Mutex::new(HashMap::from([
            (context, vec![(1, Arc::downgrade(&one))]),
        ])));
        let worker = start_shutdown_thread(instances.clone(), receiver);
        let backend = Arc::new(GStreamerBackend { capture_mocking: AtomicBool::new(false),
            instances: instances.clone(), next_instance_id: AtomicUsize::new(3),
            backend_chan: Arc::new(Mutex::new(sender)), });
        let action_backend = backend.clone();
        let (entered, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let action = thread::spawn(move || action_backend.media_instance_action(&context, &|instance| {
            assert_eq!(instance.get_id(), 1);
            entered.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(())
        }));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let registry_available = if let Ok(mut map) = instances.try_lock() {
            map.get_mut(&context).unwrap().push((2, Arc::downgrade(&two)));
            true
        } else { false };
        drop(one);
        release.send(()).unwrap();
        action.join().unwrap();
        let acknowledged = result.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(backend);
        worker.join().unwrap();
        let remaining = instances.lock().unwrap().get(&context)
            .map(|entries| entries.iter().map(|entry|entry.0).collect::<Vec<_>>());
        assert!(registry_available && acknowledged && remaining == Some(vec![2]),
            "registry_available={registry_available}, acknowledged={acknowledged}, remaining={remaining:?}");
    }
}
