// SPDX-License-Identifier: GPL-3.0-or-later
//! Keep navigation downloads on disk and hand off only a completed response.
use std::io::Write;
use servo::{ExternalDocumentEvent, ExternalDocumentId};
use crate::downloads::{PendingFile, ReadyDownload, filename};

#[derive(Default)]
pub struct Incoming { pending: Option<(ExternalDocumentId, ReadyDownload)> }
impl Incoming {
    pub fn cancel(&mut self) { self.pending.take(); }
    pub fn event(&mut self, event: ExternalDocumentEvent) -> Result<Option<ReadyDownload>, String> {
        let result = (|| {
            match event {
                ExternalDocumentEvent::Start {id, url, content_type, content_disposition} => {
                    self.cancel();
                    self.pending = Some((id, ReadyDownload {
                        file: PendingFile::staged().map_err(|error| error.to_string())?,
                        name: filename(&url, &content_disposition, &content_type), mime: content_type,
                    }));
                },
                ExternalDocumentEvent::Data {id, bytes} => {
                    if let Some((current, download)) = &mut self.pending && *current == id {
                        download.file.file.write_all(&bytes).map_err(|error| error.to_string())?;
                    }
                },
                ExternalDocumentEvent::Finished {id, result} => {
                    if self.pending.as_ref().is_some_and(|(current, _)| *current == id) {
                        let (_, download) = self.pending.take().unwrap();
                        result?;
                        return Ok(Some(download));
                    }
                },
            }
            Ok(None)
        })();
        if result.is_err() { self.cancel(); }
        result
    }
}
