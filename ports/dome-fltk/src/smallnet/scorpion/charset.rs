// SPDX-License-Identifier: GPL-3.0-or-later
//! Font fallback for Scorpion's byte-oriented character sets. Original files
//! remain unchanged; this renderer uses UTF-8 because Servo text uses Unicode.
use super::super::Error;
use std::ffi::CStr;

unsafe extern "C" {
    fn iconv_open(to:*const libc::c_char,from:*const libc::c_char)->*mut libc::c_void;
    fn iconv_close(cd:*mut libc::c_void)->libc::c_int;
    fn iconv(cd:*mut libc::c_void,input:*mut *mut libc::c_char,left:*mut usize,output:*mut *mut libc::c_char,available:*mut usize)->usize;
}
pub(super) struct Converter(*mut libc::c_void);
impl Converter {
    fn new(name:&CStr)->Result<Self,Error> {
        // iconv owns its descriptor until Drop. Each decoder is used locally
        // and is never shared with another thread or document block.
        let descriptor=unsafe{iconv_open(c"UTF-8".as_ptr(),name.as_ptr())};
        if descriptor as isize == -1{return Err(Error::Unsupported);}
        Ok(Self(descriptor))
    }
    fn convert(&mut self,bytes:&[u8])->Result<String,Error> {
        let mut input=bytes.as_ptr() as *mut libc::c_char;let mut left=bytes.len();let mut result=Vec::new();
        while left!=0 {
            let mut buffer=[0u8;512];let mut output=buffer.as_mut_ptr().cast();let mut available=buffer.len();
            let before=left;
            let status=unsafe{iconv(self.0,&mut input,&mut left,&mut output,&mut available)};
            result.extend_from_slice(&buffer[..buffer.len()-available]);
            if status==usize::MAX && std::io::Error::last_os_error().raw_os_error()!=Some(libc::E2BIG){return Err(Error::Unsupported);}
            if before==left && available==buffer.len(){return Err(Error::Response);}
        }
        String::from_utf8(result).map_err(|_|Error::Response)
    }
}
impl Drop for Converter {fn drop(&mut self){unsafe{iconv_close(self.0);}}}
fn convert(name:&CStr,bytes:&[u8])->Result<String,Error>{Converter::new(name)?.convert(bytes)}

pub(super) enum Decoder {Pc(Converter),Tron{plane:Option<u16>},Iso(Converter)}
impl Decoder {
    pub(super) fn new(encoding:u8,body:&[u8])->Result<Self,Error> {
        match encoding {
            0x10=>Ok(Self::Pc(Converter::new(c"CP437")?)),
            0x00|0x80=>Ok(Self::Tron{plane:None}),
            0x20|0xa0=>{
                // The draft permits partial ISO-2022 implementations. iconv
                // validates the JP-2/JP-3 designations, including their state.
                let name=if body.windows(4).any(|w|matches!(w,b"\x1b$(O"|b"\x1b$(P"|b"\x1b$(Q")){c"ISO-2022-JP-3"}else{c"ISO-2022-JP-2"};
                Ok(Self::Iso(Converter::new(name)?))
            },
            _=>Err(Error::Unsupported),
        }
    }
    pub(super) fn graphic(&mut self,bytes:&[u8])->Result<String,Error> {
        match self {
            Self::Pc(converter)=>{
                let mut result=String::new();let mut start=0;
                for (i,b) in bytes.iter().enumerate(){if *b==0x7f{result.push_str(&converter.convert(&bytes[start..i])?);result.push('⌂');start=i+1;}}
                result.push_str(&converter.convert(&bytes[start..])?);Ok(result)
            },
            Self::Iso(converter)=>{
                let mut result=String::new();let mut start=0;let mut i=0;
                while i<bytes.len(){
                    if matches!(bytes[i],0x8e|0x8f){
                        result.push_str(&converter.convert(&bytes[start..i])?);
                        let next=*bytes.get(i+1).filter(|b|**b>=0xa0).ok_or(Error::Response)?;
                        result.push_str(&converter.convert(&[0x1b,if bytes[i]==0x8e{b'N'}else{b'O'},next&0x7f])?);i+=2;start=i;
                    }else{i+=1;}
                }
                result.push_str(&converter.convert(&bytes[start..])?);Ok(result)
            },
            Self::Tron{plane}=>{
                let mut result=String::new();let mut i=0;
                while i<bytes.len(){
                    if bytes[i]==0x20{result.push(' ');i+=1;continue;}
                    if bytes[i]==0xfe {
                        let mut volume=0u16;i+=1;
                        while bytes.get(i)==Some(&0xfe){volume=volume.checked_add(1).filter(|v|*v<=255).ok_or(Error::Response)?;i+=1;}
                        let p=*bytes.get(i).filter(|b|valid(**b)).ok_or(Error::Response)?;i+=1;*plane=Some((volume<<8)|p as u16);continue;
                    }
                    let lo=bytes[i];let hi=*bytes.get(i+1).ok_or(Error::Response)?;
                    if !valid(lo) || !valid(hi){return Err(Error::Response);}
                    let plane=plane.ok_or(Error::Response)?;
                    result.push_str(&tron(plane,hi,lo)?);i+=2;
                }
                Ok(result)
            },
        }
    }
    pub(super) fn escape(&mut self,bytes:&[u8])->Result<String,Error> {
        match self {
            Self::Iso(converter)=>{
                if !matches!(bytes,b"\x1b(B"|b"\x1b(J"|b"\x1b(I"|b"\x1b$@"|b"\x1b$A"|b"\x1b$B"|b"\x1b$(D"|b"\x1b$(O"|b"\x1b$(P"|b"\x1b$(Q"|b"\x1b.A"|b"\x1b.F"){return Err(Error::Unsupported);}
                let text=converter.convert(bytes)?;
                if text.contains('\x1b'){return Err(Error::Unsupported);}Ok(text)
            },
            _=>Err(Error::Response),
        }
    }
    pub(super) fn pc_graphic(code:u8)->Result<&'static str,Error> {
        const GRAPHICS:[&str;32]=["�","☺","☻","♥","♦","♣","♠","•","◘","○","◙","♂","♀","♪","♫","☼","►","◄","↕","‼","¶","§","▬","↨","↑","↓","→","←","∟","↔","▲","▼"];
        GRAPHICS.get(code as usize).copied().ok_or(Error::Response)
    }
}
fn valid(b:u8)->bool {(0x21..=0xfd).contains(&b) && b!=0x7f}
fn linear(hi:u8,lo:u8)->u32 {
    match (hi<0x80,lo<0x80) {
        (true,true)=>(hi as u32-0x21)*94+lo as u32-0x21,
        (false,true)=>8836+(hi as u32-0x80)*94+lo as u32-0x21,
        (true,false)=>20680+(hi as u32-0x21)*126+lo as u32-0x80,
        (false,false)=>32524+(hi as u32-0x80)*126+lo as u32-0x80,
    }
}
fn tron(plane:u16,hi:u8,lo:u8)->Result<String,Error> {
    let marker=||format!("⟦TRON {plane:04X}:{hi:02X}{lo:02X}⟧");
    let glyph=match plane {
        0x21=>match (hi,lo) {
            (0x21..=0x7e,0x21..=0x7e)=>convert(c"EUC-JISX0213",&[hi|0x80,lo|0x80]).ok(),
            (0x80,0x21..=0x60)=>char::from_u32(0x2800+(lo-0x21) as u32).map(String::from),
            (0x81..=0x83,0x21..=0x7e)=>{
                let n=(hi as u32-0x81)*94+lo as u32-0x21;
                (n<256).then(||0x2800+(n&7)+((n&8)<<3)+((n&0x70)>>1)+(n&0x80)).and_then(char::from_u32).map(String::from)
            },
            (0x87..=0xa0,0x21..=0x7e)=>{
                const ROW:[u8;26]=[1,3,4,5,8,12,13,14,15,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94];
                convert(c"EUC-JISX0213",&[0x8f,ROW[(hi-0x87) as usize]+0xa0,lo|0x80]).ok()
            },
            (0xa1..=0xfd,0x21..=0x7e)=>convert(c"EUC-JP",&[0x8f,hi,lo|0x80]).ok(),
            (0x21..=0x7e,0x80..=0xfd)=>{
                let n=(hi as u32-0x21)*126+lo as u32-0x80;
                if n<8836{convert(c"GB2312",&[(n/94) as u8+0xa1,(n%94) as u8+0xa1]).ok()}else{None}
            },
            (0xb7..=0xfd,0x80..=0xfd)=>{
                let n=(hi as u32-0xb7)*126+lo as u32-0x80;
                if n<8836{convert(c"EUC-KR",&[(n/94) as u8+0xa1,(n%94) as u8+0xa1]).ok()}else{None}
            },
            _=>None,
        },
        0x30|0x31=>{
            let code=linear(hi,lo)+if plane==0x31{48400}else{0};
            char::from_u32(code).filter(|_|code<=65535).filter(|c|!c.is_control()).map(String::from)
        },
        0x70=>match (hi,lo) {(0x21,0x21)=>Some(" ".into()),(0x22,0x21..=0x7e)=>Some((lo as char).to_string()),_=>None},
        _=>None,
    };
    Ok(glyph.unwrap_or_else(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pc_graphics_and_extended_characters_use_font_fallback_without_controls() {
        let mut pc=Decoder::new(0x10,b"").unwrap();assert_eq!(pc.graphic(b"Caf\x82 \xb3 \x7f").unwrap(),"Café │ ⌂");
        assert_eq!(Decoder::pc_graphic(1).unwrap(),"☺");
    }
    #[test]
    fn tron_shifts_japanese_braille_and_unicode_planes_are_block_local() {
        let mut decoder=Decoder::new(0,b"").unwrap();
        assert_eq!(decoder.graphic(b"\xfe\x21\x22\x24\x21\x80 \xfe\x30\x62\x21").unwrap(),"あ⠀ A");
        assert!(Decoder::new(0,b"").unwrap().graphic(b"\x22\x24").is_err());
        assert!(decoder.graphic(b"\xfe").is_err());
        assert!(decoder.graphic(b"\xfe\x21\x22").is_err());
    }
    #[test]
    fn iso2022_designations_are_validated_and_preserved_across_style_segments() {
        let mut decoder=Decoder::new(0x20,b"").unwrap();decoder.escape(b"\x1b$B").unwrap();
        assert_eq!(decoder.graphic(b"$\"").unwrap(),"あ");decoder.escape(b"\x1b(B").unwrap();assert_eq!(decoder.graphic(b"ABC").unwrap(),"ABC");
        assert!(decoder.escape(b"\x1b$(Z").is_err());
    }
}
