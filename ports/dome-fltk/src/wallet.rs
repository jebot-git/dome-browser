// SPDX-License-Identifier: GPL-3.0-or-later
use std::{io, path::{Path, PathBuf}};
use serde_json::{json, Value};
use url::Url;
use std::{cell::RefCell,collections::HashSet};
use sha2::{Digest,Sha256};
use crate::{storage,ui};

pub struct Wallet { folder:String, profile:PathBuf, pub enabled:bool, remembered:RefCell<HashSet<String>> }
fn token(kind:&str,origin:&str,scope:&str,user:&str,password:&str)->String {
    let hash: String = Sha256::digest(password.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    json!([kind,origin,scope,user,hash]).to_string()
}
pub fn safe_origin(origin:&str)->bool {
    Url::parse(origin).is_ok_and(|u| u.username().is_empty() && u.password().is_none() &&
        (u.scheme()=="https" || (u.scheme()=="http" && (u.host_str()==Some("localhost") ||
            u.host_str().is_some_and(|h|h.trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()))))))
}
fn call(value:Value)->Result<Value,String> {
    let text=serde_json::to_string(&value).map_err(|_|"Cannot encode wallet request")?;
    let reply=ui::string(unsafe {ui::browser_ui_wallet(ui::c(&text).as_ptr())});
    unsafe {ui::browser_ui_wallet_clear();}
    let reply:Value=serde_json::from_str(reply.as_deref().ok_or("No KWallet response")?).map_err(|_|"Invalid KWallet response")?;
    if let Some(error)=reply.get("error").and_then(Value::as_str) { Err(error.into()) } else { Ok(reply) }
}
impl Wallet {
    pub fn new(profile:&Path)->io::Result<Self> {
        let id_path=profile.join("wallet-id");
        let id=match storage::read(&id_path) {
            Ok(id) if id.len()==32 && id.chars().all(|c|c.is_ascii_hexdigit())=>id,
            Ok(_)=>return Err(io::Error::other("Invalid wallet profile identity")),
            Err(e) if e.kind()==io::ErrorKind::NotFound=>{
                let id=uuid::Uuid::new_v4().simple().to_string();storage::write(&id_path,&id)?;id
            },
            Err(e)=>return Err(e),
        };
        Ok(Self {folder:format!("Dome Browser {id}"),profile:profile.into(),enabled:!profile.join("wallet-disabled").exists(),remembered:RefCell::new(HashSet::new())})
    }
    pub fn toggle(&mut self)->io::Result<()> {
        let path=self.profile.join("wallet-disabled");
        if self.enabled {storage::write(&path,"disabled\n")?;} else if path.exists(){std::fs::remove_file(path)?;}
        self.enabled=!self.enabled;Ok(())
    }
    fn entries(&self,kind:&str,origin:&str,scope:&str)->Result<Vec<(String,String)>,String> {
        if !self.enabled || !safe_origin(origin){return Ok(vec![])}
        let reply=call(json!({"operation":"list","folder":self.folder}))?;
        let mut entries=Vec::new();
        for key in reply["keys"].as_array().ok_or("Invalid wallet entries")? {
            let Some(key)=key.as_str() else {continue};
            if let Ok(parts)=serde_json::from_str::<Vec<String>>(key) {
                if parts.len()==4 && parts[0]==kind && parts[1]==origin && parts[2]==scope {entries.push((key.into(),parts[3].clone()));}
            }
        }
        entries.sort_by(|a,b|a.1.cmp(&b.1));Ok(entries)
    }
    pub fn choose(&self,kind:&str,origin:&str,scope:&str,hint:&str)->Option<servo::AuthenticationResponse> {
        let entries=match self.entries(kind,origin,scope) {Ok(entries)=>entries,Err(e)=>{ui::status(&e);return None}};
        let entries:Vec<_>=entries.into_iter().filter(|(_,user)|hint.is_empty() || user==hint).collect();
        if entries.is_empty(){return None}
        let labels:Vec<_>=entries.iter().map(|(_,user)|format!("Fill {user} — {origin}")).collect();
        let i=unsafe{ui::browser_ui_select(ui::c(&serde_json::to_string(&labels).ok()?).as_ptr(),8,80)};
        let (key,_)=usize::try_from(i).ok().and_then(|i|entries.get(i))?;
        let reply=call(json!({"operation":"read","folder":self.folder,"key":key})).ok()?;
        let credentials:servo::AuthenticationResponse=serde_json::from_str(reply["value"].as_str()?).ok()?;
        self.remembered.borrow_mut().insert(token(kind,origin,scope,&credentials.username,&credentials.password));
        Some(credentials)
    }
    pub fn save(&self,kind:&str,origin:&str,scope:&str,username:&str,password:&str) {
        if !self.enabled || !safe_origin(origin){return}
        let token=token(kind,origin,scope,username,password);
        if self.remembered.borrow().contains(&token){return}
        if !ui::ask("Save login in KWallet",&format!("{origin}\nAccount: {username}\n\nSave this password in the encrypted KDE wallet?")){return}
        let key=json!([kind,origin,scope,username]).to_string();
        let value=json!({"username":username,"password":password}).to_string();
        match call(json!({"operation":"write","folder":self.folder,"key":key,"value":value})) {
            Ok(_)=>{self.remembered.borrow_mut().insert(token);ui::status("Login saved in KWallet");},Err(e)=>ui::status(&e),
        }
    }
    pub fn manage_form(&self,details:servo::LoginFormDetails)->Option<servo::AuthenticationResponse> {
        if !self.enabled || !safe_origin(&details.origin) ||
            Url::parse(&details.action).ok()?.origin().ascii_serialization()!=details.origin {return None}
        if let Some(password)=details.password {
            self.save("html",&details.origin,&details.action,&details.username,&password);None
        } else {self.choose("html",&details.origin,&details.action,&details.username)}
    }
    pub fn forget_site(&self,origin:&str) {
        if !self.enabled{return}
        let reply=match call(json!({"operation":"list","folder":self.folder})){Ok(v)=>v,Err(e)=>{ui::status(&e);return}};
        let entries:Vec<_>=reply["keys"].as_array().into_iter().flatten().filter_map(Value::as_str)
            .filter(|key|serde_json::from_str::<Vec<String>>(key).is_ok_and(|p|p.len()==4 && p[1]==origin)).map(str::to_owned).collect();
        if entries.is_empty(){ui::status("No saved logins for this site");return}
        if !ui::ask("Forget saved logins",&format!("Remove {} saved login(s) for {origin} from this profile's wallet folder?",entries.len())){return}
        for key in entries {if let Err(e)=call(json!({"operation":"remove","folder":self.folder,"key":key})){ui::status(&e);return}}
        self.remembered.borrow_mut().clear();
        ui::status("Saved logins removed");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_secure_origins_and_loopback_may_use_saved_passwords() {
        for origin in ["https://example.org","http://localhost:8000","http://127.0.0.1:8000","http://[::1]:8000"] {assert!(safe_origin(origin),"{origin}")}
        for origin in ["http://example.org","file:///tmp/login","data:text/html,x","https://u:p@example.org"] {assert!(!safe_origin(origin),"{origin}")}
    }
}
