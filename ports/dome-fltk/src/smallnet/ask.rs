// SPDX-License-Identifier: GPL-3.0-or-later
//! Native Gopher+ ASK descriptions and bounded, transient submissions.
use super::{Error, MAX_BYTES};
use serde::{Deserialize, Serialize};
use std::{fs::{self, OpenOptions}, io::{Read, Write}, os::unix::fs::OpenOptionsExt, path::{Path, PathBuf}};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Field {
    pub kind: String,
    pub label: String,
    pub value: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Form { pub fields: Vec<Field> }

pub(super) enum Definition { Form(Form), Reference(url::Url) }

pub(super) fn definition(text: &str, raw: bool) -> Result<Definition, Error> {
    if text.len() > 128 * 1024 { return Err(Error::TooLarge); }
    let mut reference = None;
    let mut active = false;
    let mut found = false;
    let mut explicit = false;
    for line in text.lines() {
        if line == "." { break; }
        if let Some(value) = line.strip_prefix("+ASK:") {
            if found { return Err(Error::Response); }
            found = true; active = true;
            if !value.trim().is_empty() { reference = Some(value.trim().to_owned()); }
        } else if line.starts_with('+') { active = false; }
        else if active && line.strip_prefix(' ').is_some_and(|value| !value.is_empty()) { explicit = true; }
    }
    // Explicit values take precedence over references, including when the
    // reference itself is invalid. Unknown fields still fail the whole form.
    if explicit { return parse(text).map(Definition::Form); }
    if let Some(reference) = reference {
        let target = super::document::gopher_target(&reference).ok_or(Error::Response)?;
        if !matches!(target.scheme(), "gopher" | "gopher+" | "gophers") { return Err(Error::Response); }
        let parsed = super::gopher::Request::parse(&target)?;
        if parsed.kind != b'0' || parsed.supplied.is_some() || matches!(parsed.command, Some(super::gopher::Command::Ask)) { return Err(Error::Response); }
        return Ok(Definition::Reference(target));
    }
    if raw && !found && !text.lines().any(|line| line.starts_with('+')) {
        let mut wrapped = String::from("+ASK:\r\n");
        for line in text.lines() {
            if line == "." { break; }
            wrapped.push(' '); wrapped.push_str(line.strip_prefix(' ').unwrap_or(line)); wrapped.push_str("\r\n");
        }
        return parse(&wrapped).map(Definition::Form);
    }
    Err(Error::Response)
}

pub(super) fn preview(bytes: &[u8]) -> String {
    // JSON/FLTK display data is never interpreted as a form, path or command.
    let mut result = String::new();
    for byte in bytes {
        match *byte {
            b'\n' => result.push('\n'), b'\r' => result.push_str("\\r"), b'\t' => result.push_str("\\t"),
            b' '..=b'~' => result.push(*byte as char),
            byte => result.push_str(&format!("\\x{byte:02X}")),
        }
    }
    result
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)] value: String,
    #[serde(default)] checked: bool,
    #[serde(default)] choice: usize,
}

pub(super) struct Submission {
    pub bytes: Vec<u8>,
    pub save: Option<PathBuf>,
}

fn line_value(value: &str) -> Result<&str, Error> {
    if value.len() > 65536 || value.chars().any(char::is_control) { return Err(Error::Address); }
    Ok(value)
}

pub(super) fn parse(text: &str) -> Result<Form, Error> {
    if text.len() > 128 * 1024 { return Err(Error::TooLarge); }
    let mut fields = Vec::new();
    let mut active = false;
    let mut found = false;
    for line in text.lines() {
        if line == "." { break; }
        if line.starts_with('+') {
            if line.starts_with("+ASK:") {
                if found { return Err(Error::Response); }
                found = true; active = true;
            } else { active = false; }
            continue;
        }
        if !active { continue; }
        let Some(line) = line.strip_prefix(' ') else { return Err(Error::Response); };
        if line.is_empty() { continue; }
        let (kind, rest) = line.split_once(':').ok_or(Error::Response)?;
        let mut parts = rest.trim_start_matches(' ').split('\t');
        let mut label = parts.next().unwrap_or("").to_owned();
        let extras: Vec<String> = parts.map(str::to_owned).collect();
        let kind = match kind.to_ascii_lowercase().as_str() {
            "ask" => "text", "askp" => "password", "askl" => "multiline", "askf" => "save",
            "choose" => "choice", "select" => "check", "choosef" => "file", "note" => "note",
            _ => return Err(Error::Unsupported),
        };
        let mut value = extras.first().cloned().unwrap_or_default();
        let options = if kind == "choice" {
            if extras.is_empty() || extras.len() > 128 { return Err(Error::Response); }
            extras
        } else {
            if extras.len() > 1 { return Err(Error::Response); }
            Vec::new()
        };
        if kind == "check" {
            if let Some((name, default)) = label.rsplit_once(':') {
                if matches!(default, "0" | "1") { value = default.into(); label = name.into(); }
            }
            if !matches!(value.as_str(), "" | "0" | "1") { return Err(Error::Response); }
        }
        // Filenames from metadata are suggestions, never local authority.
        if kind == "file" || kind == "password" { value.clear(); }
        if kind == "save" { value = basename(&value).unwrap_or_else(|| "download".into()); }
        if label.len() > 512 || value.len() > 4096 || options.iter().any(|s| s.len() > 512) { return Err(Error::TooLarge); }
        line_value(&label)?; line_value(&value)?;
        for option in &options { line_value(option)?; }
        fields.push(Field { kind: kind.into(), label, value, options });
        if fields.len() > 64 { return Err(Error::TooLarge); }
    }
    if !found || fields.is_empty() || fields.iter().filter(|f| f.kind == "file").count() > 1 || fields.iter().filter(|f| f.kind == "save").count() > 1 {
        return Err(Error::Response);
    }
    Ok(Form { fields })
}

fn basename(text: &str) -> Option<String> {
    let name = text.rsplit(['/', '\\']).next()?.trim();
    if name.is_empty() || matches!(name, "." | "..") || line_value(name).is_err() { None } else { Some(name.to_owned()) }
}

pub(super) fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).map_err(|_| Error::Transport)?;
    let meta = file.metadata().map_err(|_| Error::Transport)?;
    if !meta.is_file() { return Err(Error::Address); }
    if meta.len() > MAX_BYTES as u64 { return Err(Error::TooLarge); }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|_| Error::Transport)?;
    if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
    Ok(bytes)
}

impl Form {
    pub(super) fn submission(&self, json: &str) -> Result<Submission, Error> {
        if json.len() > MAX_BYTES { return Err(Error::TooLarge); }
        let answers: Vec<Answer> = serde_json::from_str(json).map_err(|_| Error::Address)?;
        if answers.len() != self.fields.len() { return Err(Error::Address); }
        let mut bytes = Vec::new();
        let mut upload = None;
        let mut save = None;
        for (field, answer) in self.fields.iter().zip(answers) {
            let value = match field.kind.as_str() {
                "note" => continue,
                "text" | "password" => line_value(&answer.value)?.to_owned(),
                "choice" => field.options.get(answer.choice).ok_or(Error::Address)?.clone(),
                "check" => if answer.checked { "1" } else { "0" }.into(),
                "multiline" => {
                    if answer.value.len() > 65536 || answer.value.chars().any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t')) { return Err(Error::Address); }
                    let normalized = answer.value.replace("\r\n", "\n").replace('\r', "\n");
                    let lines: Vec<&str> = if normalized.is_empty() { Vec::new() } else { normalized.split_terminator('\n').collect() };
                    let mut text = lines.len().to_string();
                    for line in lines { text.push_str("\r\n"); text.push_str(line); }
                    text
                }
                "file" => {
                    if answer.value.is_empty() { return Err(Error::Address); }
                    upload = Some(read_file(Path::new(&answer.value))?);
                    continue;
                }
                "save" => {
                    let path = PathBuf::from(&answer.value);
                    if !path.is_absolute() { return Err(Error::Address); }
                    let name = basename(&answer.value).ok_or(Error::Address)?;
                    save = Some(path);
                    name
                }
                _ => return Err(Error::Unsupported),
            };
            bytes.extend_from_slice(value.as_bytes()); bytes.extend_from_slice(b"\r\n");
            if bytes.len() > MAX_BYTES { return Err(Error::TooLarge); }
        }
        if let Some(upload) = upload {
            if bytes.len() + upload.len() > MAX_BYTES { return Err(Error::TooLarge); }
            bytes.extend(upload);
        }
        Ok(Submission { bytes, save })
    }
}

pub(super) fn save(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_BYTES || !path.is_absolute() { return Err(Error::Address); }
    let parent = path.parent().ok_or(Error::Address)?;
    let temporary = parent.join(format!(".dome-smallnet-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).map_err(|_| Error::Transport)?;
        file.write_all(bytes).map_err(|_| Error::Transport)?;
        file.sync_all().map_err(|_| Error::Transport)?;
        fs::rename(&temporary, path).map_err(|_| Error::Transport)
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn referenced_and_literal_form_definitions_follow_attribute_precedence() {
        let text = "+ASK: 0Form definition\t/definition\thost\t70\t+\r\n";
        match definition(text, false).unwrap() { Definition::Reference(url) => assert_eq!(url.as_str(), "gopher+://host:70/0%2Fdefinition"), _ => panic!("Expected reference") }
        let literal = format!("{text} Ask: Local definition\r\n");
        assert!(matches!(definition(&literal, false), Ok(Definition::Form(_))));
        assert!(matches!(definition("Ask: Plain referenced definition\r\nChoose: Color\tRed\tBlue\r\n", true), Ok(Definition::Form(_))));
        for text in ["+ASK: hRemote\tURL:https://example.com/\thost\t70\r\n", "+ASK: 1Directory\t/\thost\t70\t+\r\n", "+ASK:\r\n+ASK:\r\n Ask: Duplicate\r\n"] { assert!(definition(text, false).is_err()); }
    }
    #[test]
    fn forms_preserve_order_and_encode_native_control_answers() {
        let form = parse("+ASK:\r\n Note: Local form\r\n Ask: Name\tdefault\r\n AskP: Secret\tignored\r\n Choose: Color\tRed\tBlue\r\n Select: Enabled:1\r\n AskL: Lines\r\n").unwrap();
        assert_eq!(form.fields[2].value, "");
        assert_eq!(form.fields[4].value, "1");
        let submission = form.submission(r#"[{}, {"value":"世界"}, {"value":"local"}, {"choice":1}, {"checked":true}, {"value":"first\n.second\n"}]"#).unwrap();
        assert_eq!(submission.bytes, "世界\r\nlocal\r\nBlue\r\n1\r\n2\r\nfirst\r\n.second\r\n".as_bytes());
    }
    #[test]
    fn file_defaults_never_become_local_file_access() {
        let form = parse("+ASK:\n ChooseF: Upload\t/private/server/default\n AskF: Save\t../../example.txt\n").unwrap();
        assert_eq!(form.fields[0].value, "");
        assert_eq!(form.fields[1].value, "example.txt");
        assert!(form.submission(r#"[{"value":""},{"value":"relative"}]"#).is_err());
    }
    #[test]
    fn unsupported_or_ambiguous_forms_do_not_submit_partial_answers() {
        for text in ["+INFO: item\n", "+ASK:\n Unknown: input\n", "+ASK:\n Choose: Missing options\n", "+ASK:\n Select: Multiple\tA\tB\n", "+ASK:\n ChooseF: One\n ChooseF: Two\n"] {
            assert!(parse(text).is_err());
        }
        let form = parse("+ASK:\n Ask: Input\n").unwrap();
        assert!(form.submission("[]").is_err());
        assert!(form.submission(r#"[{"value":"two\nlines"}]"#).is_err());
    }
    #[test]
    fn explicit_upload_is_binary_and_save_is_atomic_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dome-ask-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("picked.bin"); fs::write(&path, b"\0.\r\nbytes").unwrap();
        let form = parse("+ASK:\n Ask: Label\n ChooseF: File\n AskF: Save\n").unwrap();
        let destination = dir.join("result.txt");
        let json = serde_json::json!([{"value":"local"},{"value":path},{"value":destination}]).to_string();
        let result = form.submission(&json).unwrap();
        assert_eq!(result.bytes, b"local\r\nresult.txt\r\n\0.\r\nbytes");
        fs::write(&destination, b"previous response").unwrap();
        save(result.save.as_ref().unwrap(), b"response\0").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"response\0");
        assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }
}
