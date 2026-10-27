// Local HTTPS fixture only. All cookie values are synthetic.
const deadline = promise => Promise.race([promise, new Promise((_, reject) =>
  setTimeout(() => reject(new Error('cookie fixture timeout')), 20000))]);
const echo = async (url = '/echo') => (await fetch(url, {credentials:'include'})).json();
const workerProbe = depth => deadline(new Promise((resolve,reject) => {
  const worker = new Worker('/worker.js');
  worker.onmessage = event => {worker.terminate();resolve(event.data)};
  worker.onerror = reject;worker.postMessage({depth});
}));
if (CFG.role === 'worker') {
  onmessage = async event => {
    try {postMessage({cookie:(await echo()).cookie, child:event.data.depth === 0 ? await workerProbe(1) : null})}
    catch(error) {postMessage({error:String(error)})}
  };
} else if (CFG.role === 'shared') {
  onconnect = event => {const port=event.ports[0];port.onmessage=async () => {
    try {port.postMessage(await echo())} catch(error) {port.postMessage({error:String(error)})}
  };port.start()};
} else {
  const ports=[];
  const sharedProbe = () => deadline(new Promise((resolve,reject) => {
    const worker=new SharedWorker('/shared.js','cookie-policy');ports.push(worker.port);
    worker.port.onmessage=event=>resolve(event.data);worker.onerror=reject;
    worker.port.start();worker.port.postMessage('probe');
  }));
  const websocket = origin => deadline(new Promise((resolve,reject) => {
    const socket=new WebSocket(origin.replace('https:','wss:')+'/ws');
    socket.onmessage=event=>{resolve(JSON.parse(event.data));socket.close()};
    socket.onerror=()=>reject(new Error('WebSocket failed'));
  }));
  const frame = (origin,label,referrerPolicy='',sandbox=false) => {
    const element=document.createElement('iframe');element.referrerPolicy=referrerPolicy;
    if(sandbox)element.sandbox='allow-scripts';
    element.src=origin+'/frame?label='+label;element.title=label;document.body.append(element);
  };
  const storeProbe = async () => {
    if (!globalThis.cookieStore) return {supported:false};
    try {return {supported:true,cookies:await cookieStore.getAll()}}
    catch(error) {return {supported:true,error:error.name}}
  };
  (async () => {
    if (CFG.role==='frame') {
      if(CFG.label==='sandbox'){
        let error='';try{document.cookie}catch(e){error=e.name}
        top.postMessage({label:'sandbox',cookieError:error,network:await echo()},'*');return;
      }
      if (CFG.label==='cross') frame(CFG.first,'nested');
      const before=document.cookie;
      document.cookie='script=fixture; Path=/; Secure; SameSite=None; Max-Age=3600';
      const result={label:CFG.label,before,after:document.cookie,network:await echo(),
        worker:await workerProbe(0),shared:await sharedProbe(),ws:await websocket(location.origin),store:await storeProbe()};
      top.postMessage(result,'*');
    } else if (CFG.role==='run') {
      // Keep a first-party named worker alive before the tainted same-origin
      // descendant asks for the same worker URL and name.
      const shared=await sharedProbe();
      const received={};
      const complete=deadline(new Promise(resolve=>addEventListener('message',event=>{
        if (!event.data || !['same','no-referrer','cross','nested','sandbox'].includes(event.data.label))return;
        const expected=event.data.label==='sandbox'?'null':event.data.label==='cross'?CFG.third:CFG.first;
        if(event.origin!==expected)return;
        received[event.data.label]=event.data;if(Object.keys(received).length===5)resolve();
      })));
      frame(CFG.first,'same');frame(CFG.first,'no-referrer','no-referrer');frame(CFG.third,'cross');
      frame(CFG.first,'sandbox','',true);
      const cross=await echo(CFG.third+'/echo');
      const redirected=await echo('/redirect');
      const authorization=await (await fetch(CFG.third+'/auth',{
        credentials:'include',headers:{Authorization:'Basic Zml4dHVyZTpmaXh0dXJl'}})).json();
      const svgImages=await Promise.all([CFG.first,CFG.third].flatMap(origin=>['none','anonymous','use-credentials'].map(async mode=>{
        const svg=document.createElementNS('http://www.w3.org/2000/svg','svg'),image=document.createElementNS('http://www.w3.org/2000/svg','image');
        image.setAttribute('width','2');image.setAttribute('height','2');
        if(mode!=='none')image.crossOrigin=mode;
        const done=deadline(new Promise(resolve=>{image.onload=()=>resolve(true);image.onerror=()=>resolve(false)}));
        image.setAttribute('href',origin+'/cookie-image.svg?phase='+encodeURIComponent(CFG.phase)+'&mode='+mode);
        svg.append(image);document.body.append(svg);const loaded=await done;svg.remove();return {origin,mode,loaded};
      })));
      const ws=await websocket(CFG.third);await complete;
      await fetch('/report',{method:'POST',body:JSON.stringify({phase:CFG.phase,top:document.cookie,
        shared,cross,redirected,authorization,ws,svgImages,frames:received})});
    } else {
      await fetch('/report',{method:'POST',body:JSON.stringify({phase:CFG.phase,seed:document.cookie})});
    }
  })().catch(error => fetch(CFG.first+'/report',{method:'POST',body:JSON.stringify({phase:CFG.phase,error:String(error)})}));
}
