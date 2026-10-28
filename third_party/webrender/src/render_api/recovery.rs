// SPDX-License-Identifier: MPL-2.0
//! Compact CPU state for reconstructing one document on a fresh renderer.
use super::*;
use std::collections::{HashMap, HashSet};

/// Why a fresh renderer cannot yet reproduce a retained document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    /// Tracking was not enabled before document creation.
    NotTracked,
    /// The requested document is absent from the retained state.
    MissingDocument,
    /// A fresh API must use the original resource namespace.
    NamespaceMismatch,
    /// The target already contains scene or resource state.
    TargetNotEmpty,
    /// Opaque blob commands or GPU snapshots need their producer to recreate them.
    ProducerResourcesPending,
    /// An instance refers to a font whose template has already been removed.
    FontResourcesPending,
}

#[derive(Default)]
pub(super) struct RecoveryState {
    pub documents: HashMap<DocumentId, DocumentState>,
    images: HashMap<ImageKey, AddImage>,
    fonts: HashMap<FontKey, AddFont>,
    instances: HashMap<FontInstanceKey, AddFontInstance>,
    blobs: HashSet<BlobImageKey>,
    snapshots: HashSet<SnapshotImageKey>,
}

pub(super) struct DocumentState {
    root: Option<PipelineId>,
    view: Option<DeviceIntRect>,
    quality: Option<QualitySettings>,
    pipelines: HashMap<PipelineId, (Epoch, BuiltDisplayList)>,
    scrolls: HashMap<ExternalScrollId, Vec<SampledScrollOffset>>,
    transforms: HashMap<PropertyBindingId, PropertyValue<LayoutTransform>>,
    floats: HashMap<PropertyBindingId, PropertyValue<f32>>,
    colors: HashMap<PropertyBindingId, PropertyValue<api::ColorF>>,
    zooming: HashSet<PropertyBindingId>,
}

impl DocumentState {
    pub fn new(_size: DeviceIntSize) -> Self {
        Self {
            root: None,
            view: None,
            quality: None,
            pipelines: HashMap::new(),
            scrolls: HashMap::new(),
            transforms: HashMap::new(),
            floats: HashMap::new(),
            colors: HashMap::new(),
            zooming: HashSet::new(),
        }
    }
    fn record(&mut self, transaction: &Transaction) {
        for operation in &transaction.scene_ops {
            match operation {
                SceneMsg::UpdateEpoch(id, epoch) => {
                    if let Some((current, _)) = self.pipelines.get_mut(id) {
                        *current = *epoch;
                    }
                },
                SceneMsg::SetRootPipeline(id) => self.root = Some(*id),
                SceneMsg::RemovePipeline(id) => {
                    self.pipelines.remove(id);
                    self.scrolls.retain(|key, _| key.pipeline_id() != *id);
                    if self.root == Some(*id) {
                        self.root = None;
                    }
                },
                SceneMsg::SetDisplayList {
                    pipeline_id,
                    epoch,
                    display_list,
                } => {
                    self.pipelines
                        .insert(*pipeline_id, (*epoch, display_list.clone()));
                },
                SceneMsg::SetDocumentView { device_rect } => self.view = Some(*device_rect),
                SceneMsg::SetQualitySettings { settings } => self.quality = Some(*settings),
                SceneMsg::RenderOffscreen(_) => {},
            }
        }
        for operation in &transaction.frame_ops {
            match operation {
                FrameMsg::UpdateEpoch(id, epoch) => {
                    if let Some((current, _)) = self.pipelines.get_mut(id) {
                        *current = *epoch;
                    }
                },
                FrameMsg::SetScrollOffsets(id, offsets) => {
                    self.scrolls.insert(*id, offsets.clone());
                },
                FrameMsg::ResetDynamicProperties => {
                    self.transforms.clear();
                    self.floats.clear();
                    self.colors.clear();
                },
                FrameMsg::AppendDynamicProperties(properties) => {
                    for value in &properties.transforms {
                        self.transforms.insert(value.key.id, *value);
                    }
                    for value in &properties.floats {
                        self.floats.insert(value.key.id, *value);
                    }
                    for value in &properties.colors {
                        self.colors.insert(value.key.id, *value);
                    }
                },
                FrameMsg::AppendDynamicTransformProperties(properties) => {
                    for value in properties {
                        self.transforms.insert(value.key.id, *value);
                    }
                },
                FrameMsg::SetIsTransformAsyncZooming(enabled, id) => {
                    if *enabled {
                        self.zooming.insert(*id);
                    } else {
                        self.zooming.remove(id);
                    }
                },
                // Query replies and debug-only minimap overlays are not scene state.
                FrameMsg::HitTest(..)
                | FrameMsg::RequestHitTester(..)
                | FrameMsg::SetMinimapData(..) => {},
            }
        }
    }
    fn replay(&self) -> Transaction {
        let mut transaction = Transaction::new();
        if let Some(view) = self.view {
            transaction.set_document_view(view);
        }
        if let Some(quality) = self.quality {
            transaction.set_quality_settings(quality);
        }
        for (id, (epoch, list)) in &self.pipelines {
            transaction.set_display_list(*epoch, (*id, list.clone()));
        }
        if let Some(root) = self.root {
            transaction.set_root_pipeline(root);
        }
        for (id, offsets) in &self.scrolls {
            transaction.set_scroll_offsets(*id, offsets.clone());
        }
        transaction.reset_dynamic_properties();
        transaction.append_dynamic_properties(DynamicProperties {
            transforms: self.transforms.values().copied().collect(),
            floats: self.floats.values().copied().collect(),
            colors: self.colors.values().copied().collect(),
        });
        for id in &self.zooming {
            transaction.set_is_transform_async_zooming(true, *id);
        }
        transaction
    }
}

impl RecoveryState {
    pub fn empty(&self) -> bool {
        self.images.is_empty()
            && self.fonts.is_empty()
            && self.instances.is_empty()
            && self.blobs.is_empty()
            && self.snapshots.is_empty()
            && self
                .documents
                .values()
                .all(|document| document.pipelines.is_empty())
    }
    pub fn record(&mut self, document: DocumentId, transaction: &Transaction) {
        for update in &transaction.resource_updates {
            match update {
                ResourceUpdate::AddImage(image) => {
                    self.images.insert(image.key, image.clone());
                },
                ResourceUpdate::UpdateImage(image) => {
                    if let Some(current) = self.images.get_mut(&image.key) {
                        current.descriptor = image.descriptor;
                        current.data = image.data.clone();
                    }
                },
                ResourceUpdate::DeleteImage(key) => {
                    self.images.remove(key);
                },
                ResourceUpdate::AddFont(font) => {
                    let key = match font {
                        AddFont::Raw(key, ..) | AddFont::Native(key, ..) => *key,
                    };
                    self.fonts.insert(key, font.clone());
                },
                ResourceUpdate::DeleteFont(key) => {
                    self.fonts.remove(key);
                },
                ResourceUpdate::AddFontInstance(instance) => {
                    self.instances.insert(instance.key, instance.clone());
                },
                ResourceUpdate::DeleteFontInstance(key) => {
                    self.instances.remove(key);
                },
                ResourceUpdate::AddBlobImage(image) => {
                    self.blobs.insert(image.key);
                },
                ResourceUpdate::UpdateBlobImage(image) => {
                    self.blobs.insert(image.key);
                },
                ResourceUpdate::SetBlobImageVisibleArea(key, _) => {
                    self.blobs.insert(*key);
                },
                ResourceUpdate::DeleteBlobImage(key) => {
                    self.blobs.remove(key);
                },
                ResourceUpdate::AddSnapshotImage(image) => {
                    self.snapshots.insert(image.key);
                },
                ResourceUpdate::DeleteSnapshotImage(key) => {
                    self.snapshots.remove(key);
                },
            }
        }
        if let Some(state) = self.documents.get_mut(&document) {
            state.record(transaction);
        }
    }
    pub fn replay(&self, document: DocumentId) -> Result<Transaction, RecoveryError> {
        if !self.blobs.is_empty() || !self.snapshots.is_empty() {
            return Err(RecoveryError::ProducerResourcesPending);
        }
        if self
            .instances
            .values()
            .any(|instance| !self.fonts.contains_key(&instance.font_key))
        {
            return Err(RecoveryError::FontResourcesPending);
        }
        let state = self
            .documents
            .get(&document)
            .ok_or(RecoveryError::MissingDocument)?;
        let mut transaction = state.replay();
        transaction
            .resource_updates
            .extend(self.fonts.values().cloned().map(ResourceUpdate::AddFont));
        transaction.resource_updates.extend(
            self.instances
                .values()
                .cloned()
                .map(ResourceUpdate::AddFontInstance),
        );
        transaction
            .resource_updates
            .extend(self.images.values().cloned().map(ResourceUpdate::AddImage));
        Ok(transaction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::channel::Receiver;
    use api::{ImageDescriptorFlags, ImageFormat};

    fn state() -> (RecoveryState, DocumentId) {
        let document = DocumentId::new(IdNamespace(7), 0);
        let mut state = RecoveryState::default();
        state
            .documents
            .insert(document, DocumentState::new(DeviceIntSize::new(40, 30)));
        (state, document)
    }
    fn image(key: ImageKey, byte: u8) -> AddImage {
        AddImage {
            key,
            descriptor: ImageDescriptor::new(
                1,
                1,
                ImageFormat::RGBA8,
                ImageDescriptorFlags::empty(),
            ),
            data: ImageData::new(vec![byte; 4]),
            tiling: Some(256),
        }
    }
    #[test]
    fn recovery_compacts_repeated_image_updates_and_applies_deletion() {
        let (mut state, document) = state();
        let key = ImageKey::new(IdNamespace(7), 1);
        let mut transaction = Transaction::new();
        transaction
            .resource_updates
            .push(ResourceUpdate::AddImage(image(key, 0)));
        state.record(document, &transaction);
        for byte in 0..200 {
            let mut transaction = Transaction::new();
            transaction.update_image(
                key,
                image(key, byte).descriptor,
                ImageData::new(vec![byte; 4]),
                &ImageDirtyRect::All,
            );
            state.record(document, &transaction);
        }
        assert_eq!(state.images.len(), 1);
        assert_eq!(state.images[&key].tiling, Some(256));
        match &state.images[&key].data {
            ImageData::Raw(bytes) => assert_eq!(**bytes, vec![199; 4]),
            _ => panic!("unexpected external image"),
        }
        assert_eq!(state.replay(document).unwrap().resource_updates.len(), 1);
        let mut transaction = Transaction::new();
        transaction.delete_image(key);
        state.record(document, &transaction);
        assert!(state.empty());
        assert!(state.replay(document).unwrap().resource_updates.is_empty());
    }
    #[test]
    fn recovery_keeps_latest_scene_epoch_and_clears_removed_scrolls() {
        let (mut state, document) = state();
        let pipeline = PipelineId(4, 8);
        let scroll = ExternalScrollId(9, pipeline);
        for epoch in 0..100 {
            let mut transaction = Transaction::new();
            transaction.set_display_list(Epoch(epoch), (pipeline, BuiltDisplayList::default()));
            transaction.set_root_pipeline(pipeline);
            transaction.set_scroll_offsets(
                scroll,
                vec![SampledScrollOffset {
                    offset: LayoutVector2D::new(0., epoch as f32),
                    generation: epoch as u64,
                }],
            );
            state.record(document, &transaction);
        }
        let retained = &state.documents[&document];
        assert_eq!(retained.pipelines.len(), 1);
        assert_eq!(retained.pipelines[&pipeline].0, Epoch(99));
        assert_eq!(retained.scrolls[&scroll][0].offset.y, 99.);
        let mut transaction = Transaction::new();
        transaction.remove_pipeline(pipeline);
        state.record(document, &transaction);
        let retained = &state.documents[&document];
        assert!(
            retained.pipelines.is_empty() && retained.scrolls.is_empty() && retained.root.is_none()
        );
    }
    #[test]
    fn recovery_replays_font_templates_before_instances_and_releases_deleted_resources() {
        let (mut state, document) = state();
        let font = FontKey::new(IdNamespace(7), 5);
        let instance = FontInstanceKey::new(IdNamespace(7), 6);
        let mut transaction = Transaction::new();
        transaction
            .resource_updates
            .push(ResourceUpdate::AddFont(AddFont::Raw(
                font,
                Arc::new(vec![1, 2, 3]),
                0,
            )));
        transaction
            .resource_updates
            .push(ResourceUpdate::AddFontInstance(AddFontInstance {
                key: instance,
                font_key: font,
                glyph_size: 20.,
                options: None,
                platform_options: None,
                variations: Vec::new(),
            }));
        state.record(document, &transaction);
        let replay = state.replay(document).unwrap();
        assert!(matches!(
            replay.resource_updates[0],
            ResourceUpdate::AddFont(_)
        ));
        assert!(matches!(
            replay.resource_updates[1],
            ResourceUpdate::AddFontInstance(_)
        ));
        let mut transaction = Transaction::new();
        transaction.delete_font(font);
        state.record(document, &transaction);
        assert!(matches!(
            state.replay(document),
            Err(RecoveryError::FontResourcesPending)
        ));
        let mut transaction = Transaction::new();
        transaction.delete_font_instance(instance);
        state.record(document, &transaction);
        assert!(state.empty());
    }
    #[test]
    fn recovery_resets_dynamic_properties_without_replaying_frame_requests() {
        let (mut state, document) = state();
        let key = PropertyBindingKey::<f32>::new(17);
        for value in 0..100 {
            let mut transaction = Transaction::new();
            transaction.append_dynamic_properties(DynamicProperties {
                transforms: Vec::new(),
                floats: vec![key.with(value as f32)],
                colors: Vec::new(),
            });
            transaction.generate_frame(value, true, true, RenderReasons::SCENE);
            state.record(document, &transaction);
        }
        assert_eq!(state.documents[&document].floats.len(), 1);
        assert_eq!(state.documents[&document].floats[&key.id].value, 99.);
        let replay = state.replay(document).unwrap();
        assert!(!replay.generate_frame.as_bool() && replay.notifications.is_empty());
        let mut transaction = Transaction::new();
        transaction.reset_dynamic_properties();
        state.record(document, &transaction);
        assert!(state.documents[&document].floats.is_empty());
    }
    #[test]
    fn recovery_defers_opaque_producers_until_their_resources_are_removed() {
        let (mut state, document) = state();
        let key = BlobImageKey(ImageKey::new(IdNamespace(7), 10));
        let mut transaction = Transaction::new();
        transaction
            .resource_updates
            .push(ResourceUpdate::SetBlobImageVisibleArea(
                key,
                DeviceIntRect::zero(),
            ));
        state.record(document, &transaction);
        assert!(matches!(
            state.replay(document),
            Err(RecoveryError::ProducerResourcesPending)
        ));
        let mut transaction = Transaction::new();
        transaction
            .resource_updates
            .push(ResourceUpdate::DeleteBlobImage(key));
        state.record(document, &transaction);
        assert!(state.replay(document).is_ok());
    }
    fn api(
        namespace: IdNamespace,
    ) -> (
        RenderApi,
        Receiver<ApiMsg>,
        Receiver<SceneBuilderRequest>,
        Receiver<SceneBuilderRequest>,
    ) {
        let (api_tx, api_rx) = unbounded_channel();
        let (scene_tx, scene_rx) = unbounded_channel();
        let (low_tx, low_rx) = unbounded_channel();
        let sender = RenderApiSender::new(
            api_tx,
            scene_tx,
            low_tx,
            None,
            SharedFontResources::new(IdNamespace(100)),
        );
        let mut api = sender.create_api_by_client(namespace);
        api.enable_recovery_tracking();
        api.add_document(DeviceIntSize::new(40, 30));
        (api, api_rx, scene_rx, low_rx)
    }
    #[test]
    fn recovery_preserves_resource_allocator_and_rejects_incompatible_targets() {
        let (old, _ar, _sr, _lr) = api(IdNamespace(7));
        let document = DocumentId::new(IdNamespace(7), 0);
        let mut last = old.generate_image_key();
        for _ in 0..1000 {
            last = old.generate_image_key();
        }
        let (mut fresh, _ar2, _sr2, _lr2) = api(IdNamespace(7));
        fresh.recover_document_from(&old, document).unwrap();
        let next = fresh.generate_image_key();
        assert_eq!(next.0, last.0);
        assert!(next.1 > last.1);
        let (mut wrong, _ar3, _sr3, _lr3) = api(IdNamespace(8));
        assert_eq!(
            wrong.recover_document_from(&old, document),
            Err(RecoveryError::NamespaceMismatch)
        );
        let mut transaction = Transaction::new();
        transaction.add_image(
            next,
            image(next, 1).descriptor,
            ImageData::new(vec![1; 4]),
            None,
        );
        fresh.send_transaction(document, transaction);
        assert_eq!(
            fresh.recover_document_from(&old, document),
            Err(RecoveryError::TargetNotEmpty)
        );
    }
}
