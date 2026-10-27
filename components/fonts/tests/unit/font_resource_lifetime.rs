/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use super::*;
use fonts_traits::SystemFontServiceProxySender;
use servo_base::generic_channel;
use webrender_api::IdNamespace;

const FONT_BYTES: &[u8] = include_bytes!("../support/dejavu-fonts-ttf-2.37/ttf/DejaVuSans-Bold.ttf");

fn context() -> FontContext {
    let (font_sender, _) = generic_channel::channel().unwrap();
    let (resource_sender, _) = generic_channel::channel().unwrap();
    FontContext::new(
        Arc::new(SystemFontServiceProxySender(font_sender).to_proxy()),
        CrossProcessPaintApi::dummy(),
        ResourceThreads::new(resource_sender),
    )
}

fn loaded_face() -> (LowercaseFontFamilyName, FontTemplate) {
    // The source context is deliberately destroyed before the receiving
    // document installs the returned face.
    context()
        .construct_web_font_from_data(FONT_BYTES, CSSFontFaceDescriptors::new("Lifetime"))
        .expect("bundled font must load")
}

#[test]
fn detached_buffer_fonts_do_not_accumulate_document_bytes() {
    let context = context();
    for _ in 0..16 {
        let (_, face) = context
            .construct_web_font_from_data(FONT_BYTES, CSSFontFaceDescriptors::new("Detached"))
            .unwrap();
        let Some(FontData::SharedMemory(bytes)) = face.script_font_data.as_ref() else {
            panic!("buffer face must retain its sanitized shared bytes");
        };
        let weak = Arc::downgrade(bytes);
        drop(face);
        assert!(weak.upgrade().is_none(), "dropped detached face must release its allocation");
    }
    assert_eq!(context.font_data.read().len(), 0, "never-installed faces must own their bytes");
}

#[test]
fn unpainted_face_bytes_retire_and_retained_face_can_be_readded() {
    let context = context();
    let (family, face) = loaded_face();
    let id = Uuid::new_v4();
    context.install_script_font(id, family.clone(), face.clone(), face.script_font_data.clone());
    assert_eq!(context.font_data.read().len(), 1);
    assert!(context.webrender_font_keys.read().is_empty());
    context.remove_script_font(id);
    assert_eq!(context.collect_unused_webrender_resources(false), (vec![], vec![]));
    assert!(context.font_data.read().is_empty(), "unpainted fonts also require retirement");
    context.install_script_font(id, family, face.clone(), face.script_font_data.clone());
    assert!(context.get_font_data(&face.identifier).is_some());
}

#[test]
fn collection_preserves_sibling_and_stylesheet_owners_of_same_url() {
    let context = context();
    let (family, mut face) = loaded_face();
    face.identifier = FontIdentifier::Web(ServoUrl::parse("https://example.test/shared.ttf").unwrap());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    context.install_script_font(first, family.clone(), face.clone(), face.script_font_data.clone());
    context.install_script_font(second, family.clone(), face.clone(), face.script_font_data.clone());
    context.remove_script_font(first);
    context.collect_unused_webrender_resources(false);
    assert!(context.get_font_data(&face.identifier).is_some());

    let mut css_face = face.clone();
    css_face.script_font_data = None;
    context.add_template_to_font_context(family, css_face);
    context.remove_script_font(second);
    context.collect_unused_webrender_resources(false);
    assert!(context.get_font_data(&face.identifier).is_some(), "CSS owner still needs bytes");

    context.web_fonts.write().clear();
    context.have_removed_web_fonts.store(true, Ordering::Relaxed);
    context.collect_unused_webrender_resources(false);
    assert!(context.font_data.read().is_empty(), "last unpainted owner was removed");
}

#[test]
fn painted_font_keys_and_bytes_retire_together() {
    let context = context();
    let (family, face) = loaded_face();
    let id = Uuid::new_v4();
    context.install_script_font(id, family, face.clone(), face.script_font_data.clone());
    let key = FontKey(IdNamespace(7), 1);
    context.webrender_font_keys.write().insert(face.identifier.clone(), key);
    context.remove_script_font(id);
    let (keys, instances) = context.collect_unused_webrender_resources(false);
    assert_eq!(keys, vec![key]);
    assert!(instances.is_empty());
    assert!(context.font_data.read().is_empty());
}

#[derive(Debug, MallocSizeOf)]
struct NoopObserver;

impl CspViolationHandler for NoopObserver {
    fn process_violations(&self, _: Vec<Violation>) {}
    fn clone(&self) -> Box<dyn CspViolationHandler> { Box::new(Self) }
}

impl NetworkTimingHandler for NoopObserver {
    fn submit_timing(&self, _: ServoUrl, _: ResourceFetchTiming) {}
    fn clone(&self) -> Box<dyn NetworkTimingHandler> { Box::new(Self) }
}

fn download_document() -> WebFontDocumentContext {
    let policy = Arc::new(PolicyContainer::default());
    WebFontDocumentContext {
        policy_container: policy.clone(),
        request_client: RequestClient {
            preloaded_resources: Default::default(),
            policy_container: policy,
            origin: net_traits::request::Origin::Client,
            cookie_site: None,
            cookie_client_is_null: false,
            is_nested_browsing_context: false,
            insecure_requests_policy: net_traits::request::InsecureRequestsPolicy::DoNotUpgrade,
            has_trustworthy_ancestor_origin: true,
        },
        document_url: ServoUrl::parse("https://example.test/").unwrap(),
        csp_handler: Box::new(NoopObserver),
        network_timing_handler: Box::new(NoopObserver),
    }
}

fn script_download(context: &Arc<FontContext>, received: Arc<Mutex<Vec<FontTemplate>>>) -> WebFontDownloadState {
    context.number_of_loading_web_fonts.fetch_add(1, Ordering::SeqCst);
    WebFontDownloadState::new(
        None, context.clone(), CSSFontFaceDescriptors::new("Lifetime"),
        WebFontLoadInitiator::Script(Box::new(move |_, face| received.lock().push(face.unwrap()))),
        vec![], Default::default(), download_document(),
    )
}

fn stylesheet_download(context: &Arc<FontContext>, active: bool) -> WebFontDownloadState {
    let rule = FontFaceRule::empty(Default::default());
    let shared_lock = style::shared_lock::SharedRwLock::new();
    let info = ServoArc::new(FontFaceRuleInfo {
        cascade_index: AtomicUsize::new(0),
        descriptors: rule.descriptors.clone(),
        rule: ServoArc::new(shared_lock.wrap(rule)),
    });
    if active {
        context.known_font_face_rules.lock().contents.entry(Atom::from("lifetime"))
            .or_default().push(KnownFontFaceRule { font_face_rule_entry: info.clone(), generation: false });
    }
    context.number_of_loading_web_fonts.fetch_add(1, Ordering::SeqCst);
    let weak = Arc::downgrade(context);
    WebFontDownloadState::new(
        None, context.clone(), CSSFontFaceDescriptors::new("Lifetime"),
        WebFontLoadInitiator::Stylesheet(Box::new(FontFaceRuleInitiator {
            font_face_rule: info,
            callback: Arc::new(move |event| {
                if event == WebFontLoadEvent::LoadedSuccessfully {
                    weak.upgrade().unwrap().decrement_count_of_loading_fonts_by_one();
                }
            }),
        })),
        vec![], Default::default(), download_document(),
    )
}

#[test]
fn script_only_download_retains_bytes_in_face_instead_of_document() {
    let context = Arc::new(context());
    let received = Arc::new(Mutex::new(Vec::new()));
    let url = ServoUrl::parse("https://example.test/uninstalled.ttf").unwrap();
    let state = script_download(&context, received.clone());
    context.currently_downloading_fonts.lock().insert(url.clone(), vec![state]);
    assert!(context.handle_web_font_request_succeeded(FontData::from_bytes(FONT_BYTES), url));
    assert_eq!(context.web_fonts_still_loading(), 0);
    assert_eq!(received.lock().len(), 1);
    assert!(received.lock()[0].script_font_data.is_some());
    assert!(context.font_data.read().is_empty(), "no document owner has installed this font");
}

#[test]
fn canceled_stylesheet_download_does_not_leave_cached_bytes() {
    let context = Arc::new(context());
    let url = ServoUrl::parse("https://example.test/canceled.ttf").unwrap();
    let state = stylesheet_download(&context, false);
    context.currently_downloading_fonts.lock().insert(url.clone(), vec![state]);
    assert!(context.handle_web_font_request_succeeded(FontData::from_bytes(FONT_BYTES), url));
    assert_eq!(context.web_fonts_still_loading(), 0);
    assert!(context.web_fonts.read().families.is_empty());
    assert!(context.font_data.read().is_empty(), "canceled rule cannot own bytes");
}

#[test]
fn mixed_download_preserves_css_bytes_and_independent_script_face() {
    let context = Arc::new(context());
    let received = Arc::new(Mutex::new(Vec::new()));
    let url = ServoUrl::parse("https://example.test/mixed.ttf").unwrap();
    let script = script_download(&context, received.clone());
    let stylesheet = stylesheet_download(&context, true);
    context.currently_downloading_fonts.lock().insert(url.clone(), vec![script, stylesheet]);
    assert!(context.handle_web_font_request_succeeded(FontData::from_bytes(FONT_BYTES), url));
    assert_eq!(context.web_fonts_still_loading(), 0);
    assert_eq!(context.font_data.read().len(), 1);
    let face = received.lock().pop().unwrap();
    let receiver = self::context();
    receiver.install_script_font(Uuid::new_v4(), "lifetime".into(), face.clone(), face.script_font_data.clone());
    assert!(receiver.get_font_data(&face.identifier).is_some());
    context.web_fonts.write().clear();
    context.have_removed_web_fonts.store(true, Ordering::Relaxed);
    context.collect_unused_webrender_resources(false);
    assert!(context.font_data.read().is_empty());
    assert!(receiver.get_font_data(&face.identifier).is_some(), "other document still owns its bytes");
}
