// Synthetic local HTTPS fixture for SameSite selection and receipt.
const deadline = promise => Promise.race([promise,new Promise((_,reject)=>setTimeout(()=>reject(Error('fixture timeout')),20000))]);
const echo = async url => (await fetch(url||'/echo',{credentials:'include'})).json();
const worker = depth => deadline(new Promise((resolve,reject)=>{
  const w=new Worker('/worker.js');w.onmessage=e=>{w.terminate();resolve(e.data)};w.onerror=reject;w.postMessage(depth);
}));
if(CFG.role==='worker')onmessage=async e=>postMessage({cookie:(await echo()).cookie,child:e.data===0?await worker(1):null});
else if(CFG.role==='shared')onconnect=e=>{const p=e.ports[0];p.onmessage=async()=>p.postMessage(await echo());p.start()};
else {
  const ports=[];
  const shared=()=>deadline(new Promise((resolve,reject)=>{const w=new SharedWorker('/shared.js','samesite');ports.push(w.port);w.port.onmessage=e=>resolve(e.data);w.onerror=reject;w.port.start();w.port.postMessage('probe')}));
  const ws=origin=>deadline(new Promise((resolve,reject)=>{const w=new WebSocket(origin.replace('https:','wss:')+'/ws');w.onmessage=e=>{resolve(JSON.parse(e.data));w.close()};w.onerror=()=>reject(Error('WebSocket failed'))}));
  const frame=(origin,label,policy='')=>{const f=document.createElement('iframe');f.referrerPolicy=policy;f.src=origin+'/frame?label='+label;document.body.append(f)};
  (async()=>{
    let result;
    if(CFG.role==='frame'){
      if(CFG.label==='cross')frame(CFG.first,'nested');
      const before=document.cookie;
      for(const [name,attrs]of [['strict','SameSite=Strict; Secure'],['lax','SameSite=Lax; Secure'],['none','SameSite=None; Secure'],['default','Secure']])document.cookie='js_'+name+'=fixture; Path=/; '+attrs;
      top.postMessage({label:CFG.label,before,after:document.cookie,network:await echo(),worker:await worker(0),shared:await shared(),ws:await ws(location.origin)},'*');return;
    }
    if(CFG.role==='run'){
      const frames={};const done=deadline(new Promise(resolve=>addEventListener('message',e=>{
        if(!e.data||!['same','no-referrer','cross','nested'].includes(e.data.label))return;
        if(e.origin!==(e.data.label==='cross'?CFG.third:CFG.first))return;
        frames[e.data.label]=e.data;if(Object.keys(frames).length===4)resolve();
      })));
      const firstShared=await shared();frame(CFG.first,'same');frame(CFG.first,'no-referrer','no-referrer');frame(CFG.third,'cross');
      const cross=await echo(CFG.third+'/echo');const redirect=await echo('/redirect');const back=await echo('/redirect?back=1');
      const socket=await ws(CFG.third);await done;
      result={phase:CFG.phase,top:document.cookie,cross,redirect,back,ws:socket,frames,shared:firstShared};
    }else result={phase:CFG.phase,network:CFG.network,document:document.cookie};
    await fetch('/report',{method:'POST',body:JSON.stringify(result)});
  })().catch(error=>fetch(CFG.first+'/report',{method:'POST',body:JSON.stringify({phase:CFG.phase,error:String(error)})}));
}
