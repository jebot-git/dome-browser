// SPDX-License-Identifier: GPL-3.0-or-later
//! Save a completed PDF navigation response and open the system PDF handler.
use std::{ffi::CString, io::Write, os::unix::ffi::OsStrExt, path::PathBuf,
    sync::mpsc::{self, Receiver, Sender}};
use servo::{ExternalDocumentEvent, ExternalDocumentId};
use crate::downloads::{PendingFile, downloads_directory, filename};

unsafe extern "C" { fn browser_launch_default_pdf(path: *const std::ffi::c_char) -> i32; }

fn pdf_name(url: &url::Url, disposition: &str) -> String {
    let mut name=filename(url,disposition,"application/pdf");
    if !std::path::Path::new(&name).extension().is_some_and(|ext|ext.eq_ignore_ascii_case("pdf")) {name.push_str(".pdf");}
    name
}

#[derive(Default)]
pub struct Incoming { pending: Option<(ExternalDocumentId, PendingFile)> }
impl Incoming {
    pub fn cancel(&mut self) { self.pending.take(); }
    pub fn event(&mut self,event:ExternalDocumentEvent)->Result<Option<PathBuf>,String> {
        let result=(|| {
            match event {
                ExternalDocumentEvent::Start{id,url,content_type,content_disposition}=>{
                    self.cancel();
                    if !content_type.eq_ignore_ascii_case("application/pdf") {return Err("Unsupported document type".into());}
                    let directory=downloads_directory().map_err(|e|e.to_string())?;
                    let file=PendingFile::new(&directory,&pdf_name(&url,&content_disposition)).map_err(|e|e.to_string())?;
                    self.pending=Some((id,file));
                },
                ExternalDocumentEvent::Data{id,bytes}=>{
                    if let Some((current,file))=&mut self.pending && *current==id {
                        file.file.write_all(&bytes).map_err(|e|e.to_string())?;
                    }
                },
                ExternalDocumentEvent::Finished{id,result}=>{
                    if self.pending.as_ref().is_some_and(|(current,_)|*current==id) {
                        let (_,file)=self.pending.take().unwrap();result?;
                        return file.finish().map(Some).map_err(|e|e.to_string());
                    }
                },
            }
            Ok(None)
        })();
        if result.is_err() {self.cancel();}
        result
    }
}

pub struct Handoff { sender:Sender<Result<PathBuf,String>>, pub receiver:Receiver<Result<PathBuf,String>> }
impl Handoff {
    pub fn new()->Self {let(sender,receiver)=mpsc::channel();Self{sender,receiver}}
    pub fn launch(&self,path:PathBuf) {
        let sender=self.sender.clone();
        std::thread::spawn(move|| {
            let result=(||{
                let name=CString::new(path.as_os_str().as_bytes()).map_err(|_|"Invalid PDF path".to_owned())?;
                match unsafe{browser_launch_default_pdf(name.as_ptr())} {
                    0=>Ok(path.clone()),
                    1=>Err(format!("PDF saved to {}. Choose a PDF viewer in DomeD Default Applications",path.display())),
                    2=>Err(format!("PDF saved to {}. Cannot read default application preferences",path.display())),
                    _=>Err(format!("PDF saved to {}. The default PDF viewer could not open it",path.display())),
                }
            })();
            let _=sender.send(result);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pdf_handoff_names_keep_pdf_extension() {
        let url=url::Url::parse("https://example.invalid/generated?id=7").unwrap();
        assert_eq!(pdf_name(&url,"attachment; filename*=UTF-8''report%20test.pdf"),"report test.pdf");
        assert_eq!(pdf_name(&url,"inline; filename=REPORT.PDF"),"REPORT.PDF");
        assert_eq!(pdf_name(&url,"inline; filename=../../result.txt"),"result.txt.pdf");
        assert_eq!(pdf_name(&url,""),"generated.pdf");
    }
    #[test]
    fn pdf_handoff_names_are_plain_local_filenames() {
        let url=url::Url::parse("data:application/pdf;base64,JVBERg==").unwrap();
        assert_eq!(pdf_name(&url,""),"download.pdf");
        assert_eq!(pdf_name(&url,"attachment; filename=\"../.hidden.pdf\""),"hidden.pdf");
    }
}
