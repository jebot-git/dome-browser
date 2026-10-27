// SPDX-License-Identifier: GPL-3.0-or-later
use url::Url;
use base64::{Engine as _,engine::general_purpose::STANDARD};

pub fn escape(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            _ => result.push(c),
        }
    }
    result
}

pub(super) fn page(url: &Url, body: &str) -> String {
    page_with_metadata(url, body, None, None)
}

pub(super) fn page_with_metadata(url: &Url, body: &str, language: Option<&str>, title: Option<&str>) -> String {
    let language = language.and_then(|value| value.split(',').next()).map(str::trim).unwrap_or("");
    format!("<!doctype html><html lang=\"{}\"><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>{}</title><style>body{{font-family:monospace;max-width:76ch;margin:2em auto;padding:0 1em;line-height:1.5;color:CanvasText;background:Canvas;color-scheme:light dark}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}a{{overflow-wrap:anywhere}}li{{margin:.3em 0}}.info{{list-style:none}}.gemtext pre{{white-space:pre;overflow:auto;overflow-wrap:normal}}.line{{white-space:pre-wrap;overflow-wrap:anywhere;min-height:1.5em;margin:0}}blockquote.line{{padding-left:1em;border-left:2px solid currentColor}}</style></head><body><header><p>{}</p></header><main>{body}</main></body></html>", escape(language), escape(title.unwrap_or(url.as_str())), escape(url.as_str()))
}

pub(super) fn news(target:&super::news::Target,server:&super::news::Server,reply:&super::news::Reply,settings:Option<&Url>,filter:Option<&Url>)->Result<String,super::Error> {
    use super::news::{self,Reply};
    let link=|url:&Url,label:&str|format!("<a href=\"{}\" rel=noreferrer>{}</a>",escape(url.as_str()),escape(label));
    let mut body=format!("<nav>{}",link(&target.root(),"News groups"));
    if let Some(url)=filter {body.push_str(&format!(" · {}",link(url,"Filter groups")));}
    if let Some(url)=settings {body.push_str(&format!(" · {}",link(url,"News server")));}
    body.push_str(&format!("</nav><p>Connection: {}</p>",escape(server.transport.label())));
    let title=match reply {
        Reply::Groups {groups,truncated}=>{
            body.push_str("<h1>News groups</h1>");
            if *truncated {body.push_str("<p>Showing the first 1,024 groups. Use Filter groups to narrow the catalogue.</p>");}
            if groups.is_empty(){body.push_str("<p>No matching groups.</p>");}
            body.push_str("<ul>");
            for group in groups {body.push_str(&format!("<li>{} · articles {}–{} · {}</li>",link(&target.group_url(&group.name),&group.name),group.low,group.high,escape(&group.status)));}
            body.push_str("</ul>");"News groups".into()
        },
        Reply::Overview {name,low,high,start,end,items}=>{
            let title=format!("News: {name}");body.push_str(&format!("<h1>{}</h1><p>Available range: {low}–{high}. Page: {start}–{end}.</p>",escape(&title)));
            if items.is_empty(){body.push_str("<p>No articles in this range.</p>");}
            body.push_str("<ol>");
            for item in items {
                body.push_str(&format!("<li value=\"{}\">{}<br>{} · {}",item.number,link(&target.article_url(name,item.number),&item.subject),escape(&item.author),escape(&item.date)));
                if let Some(url)=target.id_url(&item.id){body.push_str(&format!(" · {}",link(&url,"Message-ID")));}
                if !item.references.is_empty(){body.push_str(" · reply");}body.push_str("</li>");
            }
            body.push_str("</ol><nav>");
            if start>low.max(&1) {let mut older=target.group_url(name);older.query_pairs_mut().append_pair("before",&start.to_string());body.push_str(&link(&older,"Older articles"));body.push_str(" · ");}
            body.push_str(&link(&target.group_url(name),"Newest articles"));body.push_str("</nav>");title
        },
        Reply::Article(bytes)=>{
            let article=news::article(bytes)?;let title=article.subject.clone();body.push_str(&format!("<h1>{}</h1><dl><dt>From</dt><dd>{}</dd><dt>Date</dt><dd>{}</dd><dt>Message-ID</dt><dd>{}</dd></dl>",escape(&title),escape(&article.author),escape(&article.date),escape(&article.id)));
            if let news::Kind::Group(name,Some(_))=&target.kind {body.push_str(&format!("<p>{}</p>",link(&target.group_url(name),"Back to group")));}
            if let Some(parent)=article.references.split_whitespace().last().and_then(|id|target.id_url(id)){body.push_str(&format!("<p>{}</p>",link(&parent,"Parent article")));}
            body.push_str(&format!("<pre>{}</pre>",escape(&article.body)));
            if !article.attachments.is_empty(){body.push_str("<h2>Attachments</h2><ul>");
                for (i,attachment) in article.attachments.iter().enumerate(){let mut part=target.url.clone();part.set_query(None);part.set_fragment(None);part.query_pairs_mut().append_pair("part",&(i+1).to_string());body.push_str(&format!("<li>{} · {} · {} bytes</li>",link(&part,&attachment.name),escape(&attachment.mime),attachment.size));}body.push_str("</ul>");}
            title
        },
        Reply::Failure(code,message)=>{body.push_str(&format!("<h1>News server response</h1><p>NNTP {code}: {}</p>",escape(message)));if matches!(code,480..=483){body.push_str("<p>This server requires authentication. News login is not available in this build.</p>");}"News server response".into()},
        Reply::Stream(_)=>return Err(super::Error::Response),
    };
    Ok(page_with_metadata(&target.url,&body,None,Some(&title)))
}

pub fn plain(url: &Url, text: &str) -> String { page(url, &format!("<pre>{}</pre>", escape(text))) }

pub(super) fn directory(url:&Url,lookup:Option<&super::cso::Lookup>,reply:&super::cso::Reply,mut input:impl FnMut(&Url,&str)->Option<Url>)->Result<String,super::Error> {
    use super::cso::Lookup;
    let mut root=url.clone();root.set_path("/");root.set_query(None);root.set_fragment(None);
    let template=Lookup {term:String::new(),field:String::new(),returns:Vec::new()};
    let action=|query:&Lookup,label:&str,input:&mut dyn FnMut(&Url,&str)->Option<Url>|input(&query.url(&root),label).map(|target|format!("<a href=\"{}\" rel=noreferrer>{}</a>",escape(target.as_str()),escape(label))).unwrap_or_else(||escape(label));
    let mut body=format!("<nav><a href=\"{}\" rel=noreferrer>Directory server</a> · {}</nav>",escape(root.as_str()),action(lookup.unwrap_or(&template),if lookup.is_some(){"Change lookup"}else{"Search directory"},&mut input));
    let title=if reply.index {"Directory server".to_owned()}else{format!("Directory results for {}",lookup.map(|q|q.term.as_str()).unwrap_or("lookup"))};
    body.push_str(&format!("<h1>{}</h1>",escape(&title)));
    if !(200..300).contains(&reply.code) {
        let label=match reply.code {501=>"No matching entries",503..=506|516|301|302=>"Directory authentication is unavailable",400..=499=>"Directory temporarily unavailable",_=>"Directory server response"};
        body.push_str(&format!("<h2>{label}</h2>"));
    }
    body.push_str(&format!("<p>CSO {}: {}</p>",reply.code,escape(&reply.message)));
    if reply.index {
        body.push_str("<h2>Directory fields</h2><ul>");
        for field in reply.fields()? {
            body.push_str(&format!("<li><strong>{}</strong>: {}",escape(&field.name),escape(&field.description)));
            let flags=field.attributes.split_whitespace().collect::<Vec<_>>();
            if ["Lookup","Indexed"].iter().all(|flag|flags.iter().any(|value|value.eq_ignore_ascii_case(flag))) && !flags.iter().any(|value|value.eq_ignore_ascii_case("Encrypt")) {
                body.push_str(&format!(" · {}",action(&Lookup {field:field.name,..template.clone()},"Search this field",&mut input)));
            }
            body.push_str("</li>");
        }
        body.push_str("</ul>");
        if !reply.site.is_empty(){body.push_str("<h2>Server information</h2><dl>");for row in &reply.site {if let Some((_,name,value))=super::cso::structured(row){body.push_str(&format!("<dt>{}</dt><dd>{}</dd>",escape(name),escape(value)));}}body.push_str("</dl>");}
        if let Some((code,message))=&reply.site_status {if !(200..300).contains(code){body.push_str(&format!("<p>Server information: CSO {code}: {}</p>",escape(message)));}}
    }else {
        for (id,fields) in reply.records()? {
            body.push_str(&format!("<section><h2>Entry {id}</h2><dl>"));
            for (name,value,code) in fields {body.push_str(&format!("<dt>{}{}</dt><dd><pre>{}</pre></dd>",escape(name),if code==-200{String::new()}else{format!(" (CSO {})",code.abs())},escape(value)));}
            body.push_str("</dl></section>");
        }
    }
    Ok(page_with_metadata(url,&body,None,Some(&title)))
}

pub(super) fn dictionary(url:&Url,query:Option<&super::dict::Query>,reply:&super::dict::Reply,mut input:impl FnMut(&Url,&str)->Option<Url>)->String {
    use super::dict::{Kind,Query,Reply};
    let mut root=url.clone();root.set_path("/");root.set_query(None);root.set_fragment(None);
    let action=|q:Query,label:&str,input:&mut dyn FnMut(&Url,&str)->Option<Url>| {
        input(&q.url(&root),label).map(|target|format!("<a href=\"{}\" rel=noreferrer>{}</a>",escape(target.as_str()),escape(label))).unwrap_or_else(||escape(label))
    };
    let base=Query {kind:Kind::Define,word:String::new(),database:"!".into(),strategy:".".into(),nth:None};
    let mut body=format!("<nav><a href=\"{}\" rel=noreferrer>Dictionary server</a> · {} · {}</nav>",escape(root.as_str()),action(base.clone(),"Define a word",&mut input),action(Query {kind:Kind::Match,..base.clone()},"Match words",&mut input));
    if let Some(query)=query {body.push_str(&format!("<p>{}</p>",action(Query {nth:None,..query.clone()},"Change lookup",&mut input)));}
    let title=match reply {
        Reply::Index {databases,strategies}=>{
            body.push_str("<h1>DICT dictionary server</h1><h2>Databases</h2><ul>");
            for (id,description) in databases {
                body.push_str(&format!("<li>{}: {} · {}</li>",escape(id),escape(description),action(Query {database:id.clone(),..base.clone()},"Look up a word",&mut input)));
            }
            body.push_str("</ul><h2>Matching strategies</h2><ul>");
            for (id,description) in strategies {body.push_str(&format!("<li>{}: {} · {}</li>",escape(id),escape(description),action(Query {kind:Kind::Match,strategy:id.clone(),..base.clone()},"Find matching words",&mut input)));}
            body.push_str("</ul>");"Dictionary server".into()
        },
        Reply::Definitions(definitions)=>{
            let title=format!("Definitions for {}",query.map(|q|q.word.as_str()).unwrap_or("word"));body.push_str(&format!("<h1>{}</h1>",escape(&title)));
            if definitions.is_empty(){body.push_str("<p>No definition at this result number.</p>");}
            for definition in definitions {body.push_str(&format!("<section><h2>{}</h2><p>From {} ({})</p><pre>{}</pre></section>",escape(&definition.word),escape(&definition.description),escape(&definition.database),escape(&definition.text)));}title
        },
        Reply::Matches(matches)=>{
            let title=format!("Matches for {}",query.map(|q|q.word.as_str()).unwrap_or("word"));body.push_str(&format!("<h1>{}</h1><ul>",escape(&title)));
            for (database,word) in matches {
                let target=Query {word:word.clone(),database:database.clone(),..base.clone()}.url(&root);
                body.push_str(&format!("<li><a href=\"{}\" rel=noreferrer>{}</a> ({})</li>",escape(target.as_str()),escape(word),escape(database)));
            }
            body.push_str("</ul>");if matches.is_empty(){body.push_str("<p>No match at this result number.</p>");}title
        },
        Reply::Failure(code,text)=>{
            let label=match code {552=>"No matching word",550=>"Unknown database",551=>"Unknown matching strategy",530|531|532=>"Dictionary access requires authentication",420|421=>"Dictionary server unavailable",_=>"Dictionary lookup failed"};
            body.push_str(&format!("<h1>{label}</h1><p>DICT {code}</p><pre>{}</pre>",escape(text)));label.into()
        },
    };
    page_with_metadata(url,&body,None,Some(&title))
}

pub(super) fn image(url:&Url,mime:&str,bytes:&[u8],metadata:Option<(&super::scroll::Metadata,Option<&Url>)>)->String {
    let information=metadata.map(|(metadata,link)|scroll_information(url,metadata,link,false)).unwrap_or_default();
    page(url,&format!("{information}<figure><img src=\"data:{};base64,{}\" alt=\"{}\" style=\"max-width:100%;height:auto\"></figure><p>{}</p>",
        escape(mime),STANDARD.encode(bytes),escape(url.as_str()),link(url,url.as_str(),"Image resource")))
}

fn scroll_information(url:&Url,metadata:&super::scroll::Metadata,abstract_link:Option<&Url>,abstract_view:bool)->String {
    if !abstract_view && abstract_link.is_none() && metadata.author.is_empty() && metadata.published.is_empty() && metadata.modified.is_empty(){return String::new();}
    let mut body=String::from("<section aria-label=\"Resource information\"><dl>");
    for (label,value) in [("Author",&metadata.author),("Published",&metadata.published),("Modified",&metadata.modified)] {
        if !value.is_empty(){body.push_str(&format!("<dt>{label}</dt><dd>{}</dd>",escape(value)));}
    }
    body.push_str("</dl>");
    if abstract_view {
        if let Some(target)=abstract_link {body.push_str(&format!("<p><a href=\"{}\" rel=noreferrer>Open resource</a></p>",escape(target.as_str())));}
        else {body.push_str(&format!("<p>{}</p>",link(url,url.as_str(),"Open resource")));}
    }
    else if let Some(target)=abstract_link {body.push_str(&format!("<p><a href=\"{}\" rel=noreferrer>Resource abstract</a></p>",escape(target.as_str())));}
    body.push_str("</section>");body
}

pub(super) fn scroll_plain(url:&Url,text:&str,metadata:&super::scroll::Metadata,abstract_link:Option<&Url>)->String {
    page(url,&format!("{}<pre>{}</pre>",scroll_information(url,metadata,abstract_link,false),escape(text)))
}

pub(super) fn scroll(url:&Url,text:&str,language:Option<&str>,metadata:&super::scroll::Metadata,abstract_link:Option<&Url>,abstract_view:bool,mut input:impl FnMut(&Url,&str)->Option<Url>)->String {
    fn close_lists(body:&mut String,depth:&mut usize,target:usize) {
        while *depth>target {body.push_str("</li></ul>");*depth-=1;}
    }
    let mut body=scroll_information(url,metadata,abstract_link,abstract_view);
    body.push_str("<article class=gemtext>");
    let mut depth=0;let mut pre=false;let mut title=None;let mut counters=[0usize;3];
    for line in text.lines() {
        if pre {
            if line.starts_with("```"){body.push_str("</code></pre>");pre=false;}
            else{body.push_str(&escape(line));body.push('\n');}
            continue;
        }
        let escaped=line.strip_prefix('\\').filter(|line|{
            let hashes=line.bytes().take_while(|b|*b==b'#').count();let stars=line.bytes().take_while(|b|*b==b'*').count();
            (1..=5).contains(&hashes) || ((1..=4).contains(&stars) && line[stars..].starts_with([' ','\t'])) || line.starts_with(['>']) || line.starts_with("=>") || line.starts_with("=:") || line.starts_with("```") || *line=="---"
        });
        if let Some(literal)=escaped {close_lists(&mut body,&mut depth,0);body.push_str(&format!("<p class=line>{}</p>",escape(literal)));continue;}
        let stars=line.bytes().take_while(|b|*b==b'*').count();
        if (1..=4).contains(&stars) && line[stars..].starts_with([' ','\t']) {
            if stars<=depth {close_lists(&mut body,&mut depth,stars);body.push_str("</li><li>");}
            else {while depth<stars {body.push_str("<ul><li>");depth+=1;}}
            body.push_str(&escape(line[stars..].trim_start_matches([' ','\t'])));continue;
        }
        close_lists(&mut body,&mut depth,0);
        if let Some(tag)=line.strip_prefix("```") {body.push_str(&format!("<pre aria-label=\"{}\"><code>",escape(tag.trim())));pre=true;}
        else if line.trim_end()=="---" {body.push_str("<hr>");}
        else if let Some(value)=line.strip_prefix("=>") {
            let (target,label)=link_fields(value);body.push_str(&format!("<p class=line>{}</p>",link(url,target,label)));
        }else if let Some(value)=line.strip_prefix("=:") {
            let (target,label)=link_fields(value);let action=url.join(target).ok().and_then(|target|input(&target,label));
            let rendered=if let Some(action)=action {format!("<a href=\"{}\" rel=noreferrer>↪ {}</a>",escape(action.as_str()),escape(label))}
                else{format!("<span aria-disabled=true>↪ {} (input unavailable)</span>",escape(label))};
            body.push_str(&format!("<p class=line>{rendered}</p>"));
        }else {
            let level=line.bytes().take_while(|b|*b==b'#').count();
            if (1..=5).contains(&level) {
                let heading=line[level..].trim_start_matches([' ','\t']);let mut id=String::new();
                if level==1 {if title.is_none(){title=Some(heading);}counters=[0;3];id="top".into();}
                else if level<=4 {
                    counters[level-2]+=1;for counter in &mut counters[level-1..]{*counter=0;}
                    id=counters[..level-1].iter().map(usize::to_string).collect::<Vec<_>>().join(".");
                }
                body.push_str(&format!("<h{level}{}>{}</h{level}>",if id.is_empty(){String::new()}else{format!(" id=\"{id}\"")},escape(heading)));
            }else if line.starts_with('>') {
                let quotes=line.bytes().take_while(|b|*b==b'>').count().min(16);
                for _ in 0..quotes{body.push_str("<blockquote>");}body.push_str(&format!("<p class=line>{}</p>",escape(line[quotes..].trim_start_matches([' ','\t']))));for _ in 0..quotes{body.push_str("</blockquote>");}
            }else {body.push_str(&format!("<p class=line>{}</p>",if line.is_empty(){"<br>".into()}else{escape(line)}));}
        }
    }
    close_lists(&mut body,&mut depth,0);if pre{body.push_str("</code></pre>");}body.push_str("</article>");
    page_with_metadata(url,&body,language,title)
}

pub(super) fn link_target(base:&Url,target:&str)->Option<Url> {
    base.join(target).ok().filter(|u| matches!(u.scheme(), "whois" | "rwhois" | "fsp" | "titan" | "scorpion" | "scorpions" | "gopher" | "gopher+" | "gophers" | "gemini" | "mercury" | "guppy" | "dict" | "cso" | "ph" | "news" | "nntp" | "nex" | "spartan" | "scroll" | "finger" | "ftp" | "http" | "https" | "mailto" | "misfin" | "magnet"))
}

fn link(base: &Url, target: &str, label: &str) -> String {
    let resolved = link_target(base,target);
    match resolved {
        Some(url) => format!("<a href=\"{}\" rel=\"noreferrer\">{}</a>", escape(url.as_str()), escape(label)),
        None => escape(label),
    }
}

pub(super) fn ftp(url: &Url, base: &Url, entries: &[super::ftp::Entry]) -> String {
    let mut base=base.clone();
    if !base.path().ends_with('/') {base.set_path(&format!("{}/",base.path()));}
    let mut body=String::from("<h1>FTP directory</h1><ul>");
    if base.path()!="/" {
        if let Ok(parent)=base.join("../") {body.push_str(&format!("<li>{}</li>",link(&base,parent.as_str(),"Parent directory")));}
    }
    for entry in entries {
        let name=String::from_utf8_lossy(&entry.name);
        let suffix=percent_encoding::percent_encode(&entry.name,percent_encoding::NON_ALPHANUMERIC).to_string();
        let mut target=base.clone();target.set_path(&format!("{}{}{}",base.path(),suffix,if entry.directory{"/"}else{""}));
        let label=format!("{}{}",name,if entry.directory{"/"}else{""});
        body.push_str(&format!("<li>{}",link(&base,target.as_str(),&label)));
        if let Some(size)=entry.size {body.push_str(&format!(" <small>{size} bytes</small>"));}
        if let Some(modified)=&entry.modified {body.push_str(&format!(" <small>{}</small>",escape(modified)));}
        body.push_str("</li>");
    }
    body.push_str("</ul>");page(url,&body)
}

pub fn nex(url: &Url, text: &str) -> String {
    let mut body = String::from("<pre>");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("=> ") {
            let rest = rest.trim();
            let (target, label) = rest.split_once(char::is_whitespace).map(|(t, l)| (t, l.trim())).unwrap_or((rest, rest));
            body.push_str(&link(url, target, label));
        } else { body.push_str(&escape(line)); }
        body.push('\n');
    }
    body.push_str("</pre>");
    page(url, &body)
}

fn link_fields(text: &str) -> (&str, &str) {
    let text = text.trim_matches([' ', '\t']);
    let (target, label) = text.split_once([' ', '\t']).map(|(target, label)| (target, label.trim_matches([' ', '\t']))).unwrap_or((text, text));
    (target, if label.is_empty() { target } else { label })
}

/// Render line-oriented gemtext without combining text lines or fetching links.
pub fn gemtext(url: &Url, text: &str, language: Option<&str>, mut input: impl FnMut(&Url, &str) -> Option<Url>) -> String {
    let mut body = String::from("<article class=gemtext>");
    let mut preformatted = false;
    let mut list = false;
    let mut title = None;
    for line in text.lines() {
        if let Some(description) = line.strip_prefix("```") {
            if list { body.push_str("</ul>"); list = false; }
            if preformatted { body.push_str("</code></pre>"); }
            else { body.push_str(&format!("<pre aria-label=\"{}\"><code>", escape(description.trim()))); }
            preformatted = !preformatted;
            continue;
        }
        if preformatted {
            body.push_str(&escape(line));
            body.push('\n');
            continue;
        }
        if let Some(text) = line.strip_prefix("* ") {
            if !list { body.push_str("<ul>"); list = true; }
            body.push_str(&format!("<li class=line>{}</li>", escape(text)));
            continue;
        }
        if list { body.push_str("</ul>"); list = false; }
        if let Some(text) = line.strip_prefix("=>") {
            let (target, label) = link_fields(text);
            body.push_str(&format!("<p class=line>{}</p>", link(url, target, label)));
        } else if url.scheme() == "spartan" && line.starts_with("=:") {
            let (target, label) = link_fields(&line[2..]);
            let action = url.join(target).ok().and_then(|target| input(&target, label));
            let rendered = match action {
                Some(action) => format!("<a href=\"{}\" rel=noreferrer>↪ {}</a>", escape(action.as_str()), escape(label)),
                None => escape(label),
            };
            body.push_str(&format!("<p class=line>{rendered}</p>"));
        } else if line.starts_with('#') {
            let level = line.bytes().take_while(|byte| *byte == b'#').count().min(3);
            let heading = line[level..].trim_start_matches([' ', '\t']);
            if level == 1 && title.is_none() { title = Some(heading); }
            body.push_str(&format!("<h{level}>{}</h{level}>", escape(heading)));
        } else if let Some(quote) = line.strip_prefix('>') {
            body.push_str(&format!("<blockquote class=line>{}</blockquote>", escape(quote)));
        } else {
            body.push_str(&format!("<p class=line>{}</p>", if line.is_empty() { "<br>".into() } else { escape(line) }));
        }
    }
    if preformatted { body.push_str("</code></pre>"); }
    if list { body.push_str("</ul>"); }
    body.push_str("</article>");
    page_with_metadata(url, &body, language, title)
}

pub(super) fn gopher_target(descriptor: &str) -> Option<Url> { gopher_target_at(None,descriptor) }

fn gopher_target_at(base:Option<&Url>,descriptor:&str)->Option<Url> {
    let mut chars = descriptor.chars();
    let kind = chars.next()?;
    let fields: Vec<&str> = chars.as_str().split('\t').collect();
    if fields.len() < 4 { return None; }
    if kind == 'h' && fields[1].starts_with("URL:") { return Url::parse(&fields[1][4..]).ok(); }
    if !matches!(kind, '0' | '1' | '2' | '7' | '9' | '4' | '5' | '6' | 'g' | 'I' | 'h') { return None; }
    let port: u16 = fields[3].parse().ok()?;
    if port == 0 { return None; }
    let plus = fields.get(4).is_some_and(|flag| matches!(*flag, "+" | "?"));
    let mut target = Url::parse(if kind=='2' {"cso://example.invalid/"}else if plus { "gopher+://example.invalid/" } else { "gopher://example.invalid/" }).ok()?;
    let host = if fields[2].contains(':') && !fields[2].starts_with('[') { format!("[{}]", fields[2]) } else { fields[2].to_owned() };
    target.set_host(Some(&host)).ok()?;
    target.set_port(Some(port)).ok()?;
    if kind=='2' {return super::cso::Target::parse(&target).ok().map(|target|target.url);}
    let selector = percent_encoding::utf8_percent_encode(fields[1], percent_encoding::NON_ALPHANUMERIC);
    target.set_path(&format!("/{kind}{selector}"));
    if fields.get(4) == Some(&"?") {
        target=super::gopher::command_url(&target, super::gopher::Command::Ask)?;
    }
    if base.is_some_and(|base|base.scheme()=="gophers") {return super::gopher::secure_url(&target).ok();}
    Some(target)
}

pub fn gopher(url: &Url, text: &str) -> String {
    let mut body = String::from("<ul>");
    for line in text.lines() {
        if line == "." { break; }
        if line.is_empty() { continue; }
        let mut chars = line.chars();
        let kind = chars.next().unwrap();
        let fields: Vec<&str> = chars.as_str().split('\t').collect();
        let label = fields[0];
        if kind == 'i' || kind == '3' {
            body.push_str(&format!("<li class=info>{}</li>", escape(label)));
            continue;
        }
        let target = gopher_target_at(Some(url),line);
        body.push_str("<li>");
        body.push_str(&target.as_ref().map(|target| link(url, target.as_str(), label)).unwrap_or_else(|| escape(label)));
        if let Some(target) = target.filter(|target| target.scheme()=="gopher+" || (target.scheme()=="gophers" && super::gopher::Request::parse(target).is_ok_and(|r|r.command.is_some()))) {
            if let Some(info) = super::gopher::command_url(&target, super::gopher::Command::Attributes("!".into())) {
                body.push_str(" — ");
                body.push_str(&link(url, info.as_str(), "Information and views"));
            }
        }
        body.push_str("</li>");
    }
    body.push_str("</ul>");
    page(url, &body)
}

/// Metadata stays inert; alternate views and referenced attributes are explicit
/// links, and each INFO block establishes the item its following views belong to.
pub fn gopher_attributes(url: &Url, text: &str) -> String {
    let mut body = String::from("<h1>Gopher+ information</h1>");
    let mut target = url.clone();
    let mut block = String::new();
    for line in text.lines() {
        if line == "." { break; }
        if let Some(header) = line.strip_prefix('+') {
            let Some((name, value)) = header.split_once(':') else { continue; };
            block = name.to_owned();
            body.push_str(&format!("<h2>{}</h2>", escape(name)));
            let value = value.trim_start();
            if name == "INFO" {
                target = gopher_target_at(Some(url),value).filter(|u| matches!(u.scheme(), "gopher" | "gopher+" | "gophers")).unwrap_or_else(|| url.clone());
                let label = value.split('\t').next().and_then(|label| label.get(1..)).unwrap_or(value);
                body.push_str(&format!("<p>{}</p>", link(url, target.as_str(), label)));
            } else if !value.is_empty() {
                if let Some(reference) = gopher_target_at(Some(url),value) {
                    body.push_str(&format!("<p>{}</p>", link(url, reference.as_str(), "Referenced attribute")));
                } else { body.push_str(&format!("<pre>{}</pre>", escape(value))); }
            }
        } else if let Some(value) = line.strip_prefix(' ') {
            let view = (|| {
                if block != "VIEWS" { return None; }
                let view = value.split_once(':').map(|(view, _)| view).unwrap_or(value).trim();
                let command = super::gopher::Command::parse(format!("+{view}").as_bytes()).ok()?;
                if !matches!(command, super::gopher::Command::View { .. }) { return None; }
                super::gopher::command_url(&target, command)
            })();
            if let Some(view) = view {
                body.push_str(&format!("<p>{}</p>", link(url, view.as_str(), value)));
            } else { body.push_str(&format!("<pre>{}</pre>", escape(value))); }
        }
    }
    page(url, &body)
}

pub fn gopher_error(url: &Url, text: &str) -> String {
    let mut body = format!("<h1>Gopher+ server response</h1><pre>{}</pre>", escape(text));
    if let Some(descriptor) = text.strip_prefix("3 ").and_then(|text| text.lines().next()) {
        if let Some(target) = gopher_target_at(Some(url),descriptor) {
            body.push_str(&format!("<p>{}</p>", link(url, target.as_str(), "Open the new location")));
        }
    }
    page(url, &body)
}

#[cfg(test)]
mod tests {
    #[test]
    fn misfin_contact_links_remain_clickable_in_native_documents() {
        let base=Url::parse("gemini://capsule.example.invalid/").unwrap();
        let html=link(&base,"misfin://Mailbox@example.invalid","Contact <Mailbox>");
        assert!(html.contains("href=\"misfin://Mailbox@example.invalid\""));
        assert!(html.contains("Contact &lt;Mailbox&gt;"));
    }
    use super::*;
    #[test]
    fn implicit_tls_menu_and_attribute_links_keep_tls_and_explicit_urls_keep_their_scheme() {
        let base=Url::parse("gophers://capsule:70/1/").unwrap();
        let descriptor="0Text\t/file\thost\t70\t+";
        let target=gopher_target_at(Some(&base),descriptor).unwrap();
        assert_eq!(target.as_str(),"gophers://host:70/0%2Ffile%09%09%2B");
        let html=gopher(&base,&format!("{descriptor}\r\n7Search\t/find\thost\t70\r\nhWeb\tURL:https://example.org/\thost\t70\r\n"));
        assert!(html.contains("gophers://host:70/0%2Ffile%09%09%2B"));
        assert!(html.contains("gophers://host:70/7%2Ffind"));
        assert!(html.contains("https://example.org/"));
        assert!(!html.contains("href=\"gopher:"));assert!(!html.contains("href=\"gopher+:"));
        let attributes=gopher_attributes(&base,&format!("+INFO: {descriptor}\r\n+VIEWS:\r\n text/plain: <1k>\r\n"));
        assert!(attributes.contains("gophers://host:70/0%2Ffile%09%09%2Btext%2Fplain"));
        let ask=gopher_target_at(Some(&base),"0Form\t/ask\thost\t70\t?").unwrap();
        assert_eq!(ask.as_str(),"gophers://host:70/0%2Fask%09%09%3F");
    }

    #[test]
    fn directory_links_resolve_without_changing_literal_text() {
        let page = nex(&Url::parse("nex://host/docs/").unwrap(), "ASCII <art>\n=> ../about.txt About\n=> javascript:bad() inactive");
        assert!(page.contains("ASCII &lt;art&gt;"));
        assert!(page.contains("href=\"nex://host/about.txt\""));
        assert!(!page.contains("href=\"javascript:"));
    }
    #[test]
    fn gopher_menus_keep_selector_octets_and_honor_plus_flags() {
        let page = gopher(&Url::parse("gopher://host/").unwrap(), "iInformation\tfake\tnull\t0\r\n0A & B\t/a%09b\t::1\t7070\t+\r\n.\r\n0Hidden\t/x\thost\t70");
        assert!(page.contains("Information"));
        assert!(page.contains("A &amp; B"));
        assert!(page.contains("gopher+://[::1]:7070/0%2Fa%2509b"));
        assert!(page.contains("Information and views"));
        assert!(!page.contains("Hidden"));
    }

    #[test]
    fn attributes_escape_text_and_bind_each_view_to_its_own_item() {
        let url = Url::parse("gopher+://host/1dir%09%09$").unwrap();
        let html = gopher_attributes(&url, "+INFO: 0First\tone\thost\t70\t+\r\n+VIEWS:\r\n text/plain De_DE: <10k>\r\n+ABSTRACT:\r\n <script>bad()</script>\r\n+INFO: 0Second\ttwo\thost\t70\t+\r\n+VIEWS:\r\n application/gopher-menu: <20k>\r\n.\r\n+INFO: 0Hidden\tx\thost\t70");
        assert!(html.contains("/0one%09%09%2Btext%2Fplain%20De%5FDE"));
        assert!(html.contains("/0two%09%09%2Bapplication%2Fgopher%2Dmenu"));
        assert!(html.contains("&lt;script&gt;bad()&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("Hidden"));
    }

    #[test]
    fn scrolltext_renders_sections_nested_blocks_escapes_and_explicit_actions() {
        let url=Url::parse("scroll://host/page").unwrap();let metadata=super::super::scroll::Metadata {author:"A <writer>".into(),published:"2026-10-25T12:00:00Z".into(),modified:String::new()};
        let action=Url::parse("dome-smallnet-input:fixture").unwrap();
        let html=scroll(&url,"# Title\n## Section\n### Subsection\n##### Paragraph\n* Outer\n** 1. Nested\n* Sibling\n>> Quote\n---\n\\# Literal\n```plain\n<script>\n```\n=> #1.1 Section link\n=: /search Search\n=: titan://host/upload Unsupported upload",Some("en"),&metadata,Some(&action),false,|target,_|if target.scheme()=="scroll"{Some(action.clone())}else{None});
        assert!(html.contains("<title>Title</title>"));assert!(html.contains("<h3 id=\"1.1\">Subsection</h3>"));assert!(html.contains("<h5>Paragraph</h5>"));
        assert_eq!(html.matches("<ul>").count(),2);assert_eq!(html.matches("</ul>").count(),2);assert!(html.contains("1. Nested"));
        assert_eq!(html.matches("<blockquote>").count(),2);assert!(html.contains("<hr>"));assert!(html.contains("<p class=line># Literal</p>"));
        assert!(html.contains("&lt;script&gt;"));assert!(!html.contains("<script>"));assert!(html.contains("A &lt;writer&gt;"));
        assert!(html.contains("scroll://host/page#1.1"));assert!(html.contains("Resource abstract"));assert!(html.contains("aria-disabled=true"));assert!(!html.contains("href=\"titan:"));
    }

    #[test]
    fn gemtext_preserves_lines_fences_links_and_heading_semantics() {
        let url = Url::parse("gemini://host/docs/").unwrap();
        let html = gemtext(&url, "#Title\nfirst\nsecond\n\n\n=>../next.gmi Next\n* item\n> quote\n```art\n\n  <art>\n=> literal\n```ignored\n##Subheading", Some("en,fr"), |_, _| None);
        assert!(html.contains("<title>Title</title>"));
        assert!(html.contains("<html lang=\"en\">"));
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<p class=line>first</p><p class=line>second</p>"));
        assert!(html.contains("<p class=line><br></p><p class=line><br></p>"));
        assert!(html.contains("href=\"gemini://host/next.gmi\""));
        assert!(html.contains("<ul><li class=line>item</li></ul>"));
        assert!(html.contains("<pre aria-label=\"art\"><code>\n  &lt;art&gt;\n=&gt; literal\n</code></pre>"));
        assert!(html.contains("<h2>Subheading</h2>"));
        assert!(!html.contains("ignored"));
    }

    #[test]
    fn spartan_input_is_separate_from_links_and_literal_fenced_content() {
        let mut inputs = Vec::new();
        let url = Url::parse("spartan://host/docs/").unwrap();
        let html = gemtext(&url, "=:../search Search\n=>/about About\n```\n=: /literal\n", None, |target, label| {
            inputs.push((target.to_string(), label.to_string()));
            Some(Url::parse("dome-smallnet-input:local-fixture").unwrap())
        });
        assert_eq!(inputs, [("spartan://host/search".into(), "Search".into())]);
        assert!(html.contains("href=\"dome-smallnet-input:local-fixture\""));
        assert!(html.contains("href=\"spartan://host/about\""));
        assert!(html.contains("=: /literal\n</code></pre>"));
    }
}
