// Opt-in diagnostic snapshot. No input values, text content, cookies or storage.
(() => {
  'use strict';
  const safe = (read, fallback = null) => { try { return read(); } catch (_) { return fallback; } };
  const properties = ['display','visibility','opacity','position','width','height','minWidth','minHeight',
    'maxWidth','maxHeight','flexDirection','flexBasis','flexGrow','flexShrink','alignItems','justifyContent',
    'gap','padding','margin','boxSizing','overflow','color','fill','fillOpacity','fillRule',
    'stroke','strokeWidth','strokeOpacity','strokeLinecap','strokeLinejoin','strokeDasharray','strokeDashoffset','strokeMiterlimit',
    'stopColor','stopOpacity','filter','maskImage','mixBlendMode','contentVisibility',
    'clipPath','paintOrder','transform','fontFamily','fontWeight','fontSize','lineHeight',
    'top','right','bottom','left','aspectRatio','alignSelf','flexWrap','objectFit',
    'gridTemplateColumns','gridTemplateRows'];
  const vectorAttributes = ['viewBox','width','height','d','points','x','y','x1','y1','x2','y2','cx','cy','r','rx','ry',
    'gradientUnits','gradientTransform','offset','transform','fill','fill-rule','fill-opacity',
    'stroke','stroke-width','stroke-linecap','stroke-linejoin','stroke-dasharray','stroke-dashoffset',
    'stroke-miterlimit','stroke-opacity','stop-color','stop-opacity','opacity','visibility','display'];
  const rect = element => safe(() => { const r = element.getBoundingClientRect(); return [r.x,r.y,r.width,r.height]; });
  const describe = element => {
    const style = safe(() => getComputedStyle(element));
    const computed = {};
    if (style) for (const name of properties) computed[name] = safe(() => style[name]);
    const data = {tag:element.localName,namespace:element.namespaceURI,id:element.id || '',
      class:(element.getAttribute('class') || '').slice(0,512),rect:rect(element),computed};
    if (element.namespaceURI === 'http://www.w3.org/2000/svg') {
      data.attributes = {};
      for (const name of vectorAttributes) { const value = element.getAttribute(name); if (value !== null) data.attributes[name] = value; }
    }
    return data;
  };
  const elements = [...document.querySelectorAll('svg,h1')].slice(0,160).map(element => {
    const data = describe(element);
    data.ancestors = [];
    for (let parent = element.parentElement; parent && data.ancestors.length < 8; parent = parent.parentElement)
      data.ancestors.push(describe(parent));
    if (element.namespaceURI === 'http://www.w3.org/2000/svg') {
      data.viewBox = element.getAttribute('viewBox');
      const descendants = [...element.querySelectorAll('*')].slice(0,80);
      data.descendants = descendants.map(child => ({...describe(child),parent:descendants.indexOf(child.parentElement)}));
    }
    return data;
  });
  const stylesheets = [...document.styleSheets].map(sheet => ({
    href:safe(() => { if (!sheet.href) return null; const url = new URL(sheet.href); return url.origin + url.pathname; }),
    disabled:sheet.disabled,rules:safe(() => sheet.cssRules.length)}));
  // Image diagnosis records geometry/format/origin only, never source URLs,
  // alt text or signed query values. Anonymous byte probes are restricted to
  // failed public Discord CDN assets whose query keys are not credentials.
  const imageElements = [...document.images].filter(img => img.currentSrc || img.getAttribute('src'));
  const visibleImage = element => { const r = rect(element); return r && r[2] > 0 && r[3] > 0 && r[0] < innerWidth && r[1] < innerHeight && r[0]+r[2] > 0 && r[1]+r[3] > 0; };
  imageElements.sort((a,b) => Number(visibleImage(b))-Number(visibleImage(a)));
  const images = imageElements.slice(0,256).map(element => {
    const source = safe(() => new URL(element.currentSrc || element.src, document.baseURI));
    return {rect:rect(element),complete:element.complete,
      naturalSize:[element.naturalWidth,element.naturalHeight],
      sourceOrigin:source?.origin || null,
      queryKind:!source?.search ? 'none' : [...source.searchParams.keys()].every(key => ['size','quality','format','width','height','keep_aspect_ratio','lossless','animated'].includes(key)) ? 'image-options' : 'other',
      format:source ? (source.pathname.match(/\.(png|jpe?g|gif|webp|avif|svg|ico)$/i)?.[1]?.toLowerCase() || null) : null};
  });
  const imageAudit = globalThis.__domeImageAudit || (globalThis.__domeImageAudit = {seen:new Set(),results:[]});
  if (imageAudit.results.length < 4) {
    for (const element of imageElements.filter(img => img.complete && !img.naturalWidth).slice(0,32)) {
      const source = safe(() => new URL(element.currentSrc || element.src, document.baseURI));
      if (!source || source.protocol !== 'https:' || source.username || source.password ||
          !['cdn.discordapp.com','media.discordapp.net'].includes(source.hostname) ||
          [...source.searchParams.keys()].some(key => !['size','quality','format','width','height','keep_aspect_ratio','lossless','animated'].includes(key))) continue;
      if (imageAudit.seen.has(source.href)) continue;
      imageAudit.seen.add(source.href);
      const result = {sourceOrigin:source.origin,rect:rect(element),anonymousCorsProbe:true};
      imageAudit.results.push(result);
      fetch(source.href,{mode:'cors',credentials:'omit'}).then(async response => {
        result.status = response.status;
        result.contentType = response.headers.get('content-type');
        const reader = response.body?.getReader();
        if (reader) {
          try {
            const chunk = await reader.read();
            if (chunk.value) result.magic = [...chunk.value.slice(0,24)].map(v => v.toString(16).padStart(2,'0')).join('');
          } finally { await reader.cancel(); }
        }
      }).catch(() => { result.fetchFailed = true; });
      if (imageAudit.results.length >= 4) break;
    }
  }
  const mediaProbe = document.createElement('video');
  const mediaTypes = ['video/mp4; codecs="avc1.42E01E, mp4a.40.2"',
    'video/mp4; codecs="avc1.64001f, mp4a.40.2"','video/webm; codecs="vp9, opus"',
    'application/vnd.apple.mpegurl','application/x-mpegURL'];
  const mediaCapabilities = {
    mediaSource:typeof globalThis.MediaSource,sourceBuffer:typeof globalThis.SourceBuffer,
    managedMediaSource:typeof globalThis.ManagedMediaSource,
    videoDecoder:typeof globalThis.VideoDecoder,
    canPlayType:Object.fromEntries(mediaTypes.map(type => [type,safe(() => mediaProbe.canPlayType(type))]))
  };
  const media = [...document.querySelectorAll('video,audio')].slice(0,32).map(element => ({
    tag:element.localName,rect:rect(element),readyState:safe(() => element.readyState),
    networkState:safe(() => element.networkState),paused:safe(() => element.paused),
    seeking:safe(() => element.seeking),ended:safe(() => element.ended),
    loop:safe(() => element.loop),muted:safe(() => element.muted),
    autoplay:safe(() => element.autoplay),playbackRate:safe(() => element.playbackRate),
    errorCode:safe(() => element.error && element.error.code),
    dimensions:safe(() => [element.videoWidth,element.videoHeight]),
    currentTime:safe(() => element.currentTime),
    duration:safe(() => Number.isFinite(element.duration) ? element.duration : String(element.duration)),
    buffered:safe(() => Array.from({length:Math.min(element.buffered.length,16)},
      (_,i) => [element.buffered.start(i),element.buffered.end(i)])),
    // Origins and schemes suffice for diagnosis; do not record signed URLs.
    sourceOrigin:safe(() => element.currentSrc ? new URL(element.currentSrc).origin : null),
    sourceScheme:safe(() => element.currentSrc ? new URL(element.currentSrc).protocol : null),
    sourceTypes:[...element.querySelectorAll('source')].map(source => source.type)
  }));
  const mediaContainers = [...document.querySelectorAll('[data-testid*="video"],[data-testid*="Video"]')]
    .slice(0,32).map((element,index) => {
      const data={testId:element.getAttribute('data-testid'),...describe(element)};
      if(index<8){
        data.ancestors=[];
        for(let parent=element.parentElement;parent&&data.ancestors.length<16;parent=parent.parentElement)
          data.ancestors.push(describe(parent));
      }
      return data;
    });
  return JSON.stringify({version:2,scope:'structure and computed styles; no content or input values',
    origin:location.origin,path:location.pathname,sampleTime:performance.now(),readyState:document.readyState,compatMode:document.compatMode,
    theme:document.documentElement.getAttribute('data-theme'),
    prefersDark:matchMedia('(prefers-color-scheme: dark)').matches,
    viewport:[innerWidth,innerHeight],scroll:[scrollX,scrollY],devicePixelRatio,
    fontStatus:safe(() => document.fonts.status),nodeCount:document.querySelectorAll('*').length,
    stylesheets,elements,images,imageAudit:imageAudit.results,mediaCapabilities,media,mediaContainers});
})();
