// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded Scorpion block decoding. All returned markup is generated locally.
use super::charset::Decoder;
use super::super::{Error,MAX_BYTES,document::escape};
const MAX_BLOCKS:usize=8192;
const MAX_RENDERED:usize=64*1024*1024;

pub(super) struct Block {pub kind:u8,pub rtl:bool,pub attribute:Vec<u8>,pub html:String,pub text:String}
pub(super) struct Document {pub blocks:Vec<Block>,pub fallback:bool}

pub(super) fn parse(bytes:&[u8])->Result<Document,Error> {
    if bytes.len()>MAX_BYTES{return Err(Error::TooLarge);}
    let mut blocks=Vec::new();let mut position=0usize;let mut count=0usize;let mut output=0usize;let mut fallback=false;let mut previous=None;
    while position<bytes.len() {
        count+=1;if count>MAX_BLOCKS{return Err(Error::TooLarge);}
        let header=bytes.get(position..position+3).ok_or(Error::Response)?;
        let code=header[0];let kind=code&15;let length=u16::from_be_bytes([header[1],header[2]]) as usize;position+=3;
        let attribute=bytes.get(position..position.checked_add(length).ok_or(Error::TooLarge)?).ok_or(Error::Response)?.to_vec();position+=length;
        let length=bytes.get(position..position+3).ok_or(Error::Response)?;position+=3;
        let length=((length[0] as usize)<<16)|((length[1] as usize)<<8)|length[2] as usize;
        let body=bytes.get(position..position.checked_add(length).ok_or(Error::TooLarge)?).ok_or(Error::Response)?;position+=length;
        if kind==0x0b && previous.is_some_and(|kind|!matches!(kind,8..=11)){return Err(Error::Response);}
        previous=Some(kind);
        if matches!(kind,7|14|15){continue;}
        if matches!(kind,0|12) && !attribute.is_empty(){return Err(Error::Response);}
        let encoding=code&0xf0;
        let (html,text,used_fallback)=inline(body,encoding,kind==13)?;
        fallback|=used_fallback;
        output=output.checked_add(html.len()).ok_or(Error::TooLarge)?;
        if output>MAX_RENDERED{return Err(Error::TooLarge);}
        blocks.push(Block{kind,rtl:code&0x80!=0,attribute,html,text});
    }
    Ok(Document{blocks,fallback})
}

#[derive(Clone,Copy,PartialEq)]
enum Style {Normal,Strong,Emphasis,Fixed}
fn append(html:&mut String,plain:&mut String,text:&str,style:Style,reversed:bool,rtl:bool)->Result<(),Error> {
    plain.push_str(text);let text=escape(text);
    let css=match style{Style::Normal=>"",Style::Strong=>"font-weight:bold;",Style::Emphasis=>"font-style:italic;",Style::Fixed=>"font-family:monospace;"};
    if style==Style::Normal && !reversed {html.push_str(&text);}
    else {html.push_str(&format!("<span dir={} style=\"{css}unicode-bidi:isolate\">{text}</span>",if rtl^reversed{"rtl"}else{"ltr"}));}
    if html.len()>MAX_RENDERED || plain.len()>MAX_RENDERED{return Err(Error::TooLarge);}
    Ok(())
}
fn inline(body:&[u8],encoding:u8,pre:bool)->Result<(String,String,bool),Error> {
    let mut decoder=Decoder::new(encoding,body)?;let mut html=String::new();let mut text=String::new();
    let mut i=0;let mut style=Style::Normal;let mut reversed=false;let mut data=None;let mut ruby=false;
    while i<body.len() {
        if body[i]>=0x20 && !(body[i]==0x7f && matches!(encoding,0|0x80)) {
            let start=i;
            while i<body.len() && body[i]>=0x20 && !(body[i]==0x7f && matches!(encoding,0|0x80)){i+=1;}
            let value=decoder.graphic(&body[start..i])?;append(&mut html,&mut text,&value,style,reversed,encoding&0x80!=0)?;continue;
        }
        let control=body[i];i+=1;
        match control {
            0x02=>{if !text.ends_with(' ') {append(&mut html,&mut text," ",style,reversed,encoding&0x80!=0)?;}},
            0x05=>{
                if data.is_some() || ruby{return Err(Error::Response);}
                data=Some((style,reversed));
                let end=body[i..].iter().position(|b|*b==6).map(|n|i+n).ok_or(Error::Response)?;
                if body[i..end].is_empty() || body[i..end].iter().any(|b|*b<32 || *b==127){return Err(Error::Response);}
                i=end+1;
            },
            0x06=>return Err(Error::Response),
            0x07=>{let saved=data.take().ok_or(Error::Response)?;style=saved.0;reversed=saved.1;},
            0x09|0x0a=>{if !pre{return Err(Error::Response);}append(&mut html,&mut text,if control==9{"\t"}else{"\n"},style,reversed,encoding&0x80!=0)?;},
            0x10=>{
                if encoding!=0x10{return Err(Error::Response);}
                let next=*body.get(i).filter(|b|(0x41..=0x5f).contains(*b)).ok_or(Error::Response)?;i+=1;
                append(&mut html,&mut text,Decoder::pc_graphic(next-0x40)?,style,reversed,false)?;
            },
            0x11=>style=Style::Normal,
            0x12=>style=Style::Strong,
            0x13=>style=Style::Emphasis,
            0x14=>style=Style::Fixed,
            0x15=>reversed=false,
            0x16=>reversed=true,
            0x17=>{if ruby || data.is_some(){return Err(Error::Response);}ruby=true;},
            0x18=>{
                if !ruby{return Err(Error::Response);}
                // Minimal clients display the main text and omit the reading.
                // Encoded characters cannot contain the terminating C0 byte.
                let end=body[i..].iter().position(|b|*b==0x19).map(|n|i+n).ok_or(Error::Response)?;
                i=end+1;ruby=false;
            },
            0x19=>return Err(Error::Response),
            0x1b if matches!(encoding,0x20|0xa0)=>{
                let start=i-1;while body.get(i).is_some_and(|b|(0x20..=0x2f).contains(b)){i+=1;}
                if !body.get(i).is_some_and(|b|(0x30..=0x7e).contains(b)){return Err(Error::Response);}i+=1;
                let value=decoder.escape(&body[start..i])?;append(&mut html,&mut text,&value,style,reversed,encoding&0x80!=0)?;
            },
            0x1b=>{
                if !pre || body.get(i)!=Some(&b'['){return Err(Error::Response);}i+=1;
                while body.get(i).is_some_and(|b|b.is_ascii_digit() || *b==b';'){i+=1;}
                if body.get(i)!=Some(&b'm'){return Err(Error::Response);}i+=1;
            },
            _=>{},
        }
    }
    if data.is_some() || ruby{return Err(Error::Response);}
    Ok((html,text,matches!(encoding,0|0x80)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn block(code:u8,attr:&[u8],body:&[u8])->Vec<u8>{let mut b=vec![code,(attr.len()>>8)as u8,attr.len()as u8];b.extend_from_slice(attr);b.extend_from_slice(&[((body.len()>>16)&255)as u8,((body.len()>>8)&255)as u8,(body.len()&255)as u8]);b.extend_from_slice(body);b}
    #[test]
    fn frames_and_inline_controls_preserve_visible_text_without_interpreting_markup() {
        let b=block(0x10,b"",b"Number\x02one \x12bold\x11 <literal>& \x10A");let d=parse(&b).unwrap();
        assert_eq!(d.blocks[0].text,"Number one bold <literal>& ☺");assert!(d.blocks[0].html.contains("font-weight:bold"));assert!(d.blocks[0].html.contains("&lt;literal&gt;&amp;"));
    }
    #[test]
    fn data_metadata_and_furigana_are_not_displayed_as_body_text() {
        let d=parse(&block(0x10,b"",b"A \x050latin\x06text\x07 \x17base\x18reading\x19")).unwrap();assert_eq!(d.blocks[0].text,"A text base");
        for body in [b"\x05unfinished".as_slice(),b"\x06orphan",b"\x07orphan",b"\x18orphan",b"\x17unfinished"]{assert!(parse(&block(0x10,b"",body)).is_err());}
    }
    #[test]
    fn preformatted_spacing_and_ignored_sgr_do_not_become_terminal_commands() {
        let d=parse(&block(0x1d,b"",b"one\tcell\n\x1b[31mred\x1b[0m")).unwrap();assert_eq!(d.blocks[0].text,"one\tcell\nred");
        assert!(parse(&block(0x10,b"",b"bad\nline")).is_err());assert!(parse(&block(0x1d,b"",b"\x1b[31")).is_err());
    }
    #[test]
    fn truncated_blocks_and_reserved_attributes_fail_without_partial_documents() {
        let bytes=block(0x10,b"",b"text");for n in 1..bytes.len(){assert!(parse(&bytes[..n]).is_err(),"{n}");}
        assert!(parse(&block(0x10,b"attribute",b"text")).is_err());
        let mut bytes=block(0x10,b"",b"text");bytes.extend(block(0x1b,b"other",b"alternate"));assert!(parse(&bytes).is_err());
        assert!(parse(&block(0x1b,b"other",b"alternate")).is_ok());
    }
    #[test]
    fn unknown_metadata_blocks_are_skipped_and_text_shifts_reset_per_block() {
        let mut bytes=block(0xff,b"\xc0",b"arbitrary bytes");bytes.extend(block(0x10,b"",b"visible"));let d=parse(&bytes).unwrap();assert_eq!(d.blocks.len(),1);assert_eq!(d.blocks[0].text,"visible");
        let mut bytes=block(0x00,b"",b"\xfe\x21\x22\x24");bytes.extend(block(0x00,b"",b"\x22\x24"));assert!(parse(&bytes).is_err());
    }
    #[test]
    fn unicode_fallback_and_rtl_flags_are_explicit_in_decoded_blocks() {
        let d=parse(&block(0x80,b"",b"\xfe\x30\x62\x21\x16\x63\x21\x15")).unwrap();assert!(d.fallback);assert!(d.blocks[0].rtl);assert_eq!(d.blocks[0].text,"AB");assert!(d.blocks[0].html.contains("dir=ltr"));
    }
}

fn attribute_url(base:&url::Url,attribute:&[u8])->Result<Option<url::Url>,Error> {
    let attribute=attribute.split(|b|*b==0).next().unwrap_or_default();
    if !attribute.is_ascii() || attribute.iter().any(|b|*b<=32 || *b==127){return Err(Error::Response);}
    let text=std::str::from_utf8(attribute).map_err(|_|Error::Response)?;
    Ok(super::super::document::link_target(base,text))
}
fn anchor(attribute:&[u8])->Result<String,Error> {
    let value=std::str::from_utf8(attribute).map_err(|_|Error::Response)?;
    let value=percent_encoding::percent_decode_str(value).decode_utf8().map_err(|_|Error::Response)?;
    if value.chars().any(char::is_control){return Err(Error::Response);}
    Ok(escape(&value))
}
pub(super) fn render(url:&url::Url,bytes:&[u8],mut input:impl FnMut(&url::Url,&str)->Option<url::Url>)->Result<String,Error> {
    let document=parse(bytes)?;let mut html=String::from("<article class='gemtext scorpion'>");let mut alternatives=String::new();
    let title=document.blocks.iter().find(|b|b.kind==1).map(|b|b.text.as_str());
    for block in &document.blocks {
        let dir=if block.rtl{"rtl"}else{"ltr"};
        match block.kind {
            0=>html.push_str(&format!("<p dir={dir}>{}</p>",block.html)),
            1..=6=>html.push_str(&format!("<h{0} id=\"{1}\" dir={dir}>{2}</h{0}>",block.kind,anchor(&block.attribute)?,block.html)),
            8|9|10|11=>{
                let target=attribute_url(url,&block.attribute)?;
                let label=if block.text.is_empty(){target.as_ref().map(|u|escape(u.as_str())).unwrap_or_default()}else{block.html.clone()};
                let link=if block.kind==10 {
                    format!("<span>{label} <small>(interactive session unavailable)</small></span>")
                }else if let Some(mut target)=target {
                    let target=if block.kind==9 {
                        if matches!(target.scheme(),"gopher"|"gopher+"|"gophers") {
                            let path=target.path().strip_prefix('/').unwrap_or(target.path());
                            let selector=path.get(1..).unwrap_or("");target.set_path(&format!("/7{selector}"));Some(target)
                        }else{input(&target,&block.text)}
                    }else{Some(target)};
                    match target {Some(target)=>format!("<a href=\"{}\" rel=noreferrer>{label}</a>",escape(target.as_str())),None=>format!("{label} <small>(input unavailable)</small>")}
                }else{label};
                let line=format!("<p dir={dir}>{link}</p>");
                if block.kind==11{alternatives.push_str(&line);}else{html.push_str(&line);}
            },
            12=>html.push_str(&format!("<blockquote dir={dir}>{}</blockquote>",block.html)),
            13=>html.push_str(&format!("<pre dir={dir}>{}</pre>",block.html)),
            _=>{},
        }
    }
    if !alternatives.is_empty(){html.push_str(&format!("<details><summary>Alternate services</summary>{alternatives}</details>"));}
    if document.fallback && document.blocks.iter().any(|b|b.text.contains("⟦TRON ")) {html.push_str("<p><small>Some characters are unavailable in the current font. Their codes are shown; Save Source preserves the original text.</small></p>");}
    html.push_str("</article>");
    Ok(super::super::document::page_with_metadata(url,&html,None,title))
}

#[cfg(test)]
mod rendering_tests {
    use super::*;
    fn block(kind:u8,attribute:&[u8],body:&[u8])->Vec<u8>{let mut b=vec![0x10|kind,(attribute.len()>>8)as u8,attribute.len()as u8];b.extend_from_slice(attribute);b.extend_from_slice(&[((body.len()>>16)&255)as u8,((body.len()>>8)&255)as u8,(body.len()&255)as u8]);b.extend_from_slice(body);b}
    #[test]
    fn anchors_input_links_alternates_and_interactive_links_keep_their_distinct_semantics() {
        let mut bytes=block(1,b"first%20section",b"Fixture title");bytes.extend(block(8,b"child.scorp\0ignored",b"Caf\x82"));
        bytes.extend(block(9,b"/ask",b"Input fixture"));bytes.extend(block(10,b"/interactive",b"Session fixture"));bytes.extend(block(11,b"/mirror",b"Alternate fixture"));
        let mut inputs=Vec::new();let html=render(&url::Url::parse("scorpion://host/dir/index.scorp").unwrap(),&bytes,|target,label|{inputs.push((target.clone(),label.to_string()));url::Url::parse("dome-smallnet-input:fixture").ok()}).unwrap();
        assert!(html.contains("<title>Fixture title</title>"));assert!(html.contains("id=\"first section\""));assert!(html.contains("href=\"scorpion://host/dir/child.scorp\""));assert!(html.contains("Café"));
        assert!(html.contains("href=\"dome-smallnet-input:fixture\""));assert_eq!(inputs[0].0.as_str(),"scorpion://host/ask");assert_eq!(inputs[0].1,"Input fixture");
        assert!(html.contains("interactive session unavailable"));assert!(!html.contains("href=\"scorpion://host/interactive\""));assert!(html.contains("<details><summary>Alternate services</summary>"));
    }
    #[test]
    fn a_gopher_input_link_uses_type_seven_and_a_preformatted_attribute_is_inert() {
        let mut bytes=block(9,b"gopher://host/1/query",b"Search Gopher");bytes.extend(block(13,b"unknown formatting hint",b"A  B\nC"));
        let html=render(&url::Url::parse("scorpion://host/").unwrap(),&bytes,|_,_|panic!("Gopher has its own input handling")).unwrap();
        assert!(html.contains("href=\"gopher://host/7/query\""));assert!(html.contains("<pre dir=ltr>A  B\nC</pre>"));assert!(!html.contains("unknown formatting hint"));
    }
}
