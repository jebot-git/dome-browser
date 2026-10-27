let root=document.querySelector('svg');
let title=root.querySelector('#primary');
let backup=root.querySelector('#backup');
let extra,restored,atomic;
const ns='http://www.w3.org/2000/svg';
const actions=[
 ()=>{},
 ()=>title.firstChild.data='Text mutation',
 ()=>title.textContent='Replacement title',
 ()=>document.title='API title',
 ()=>{extra=document.createElementNS(ns,'title');extra.textContent='First title';root.insertBefore(extra,title)},
 ()=>extra.remove(),
 ()=>title.remove(),
 ()=>backup.remove(),
 ()=>{restored=document.createElementNS(ns,'title');restored.textContent='Restored title';root.append(restored)},
 ()=>{const group=document.createElementNS(ns,'g');root.append(group);group.append(restored);restored.textContent='Nested text'},
 ()=>root.append(restored),
 ()=>{const clone=root.cloneNode(true);clone.querySelector('title').textContent='Root replacement';root.replaceWith(clone);root=clone},
 ()=>restored.textContent='Detached text',
 ()=>document.title='  Unicode Řeka 日本語 \n Title  ',
 ()=>{atomic=document.createElementNS(ns,'title');atomic.textContent='Atomic first';root.append(atomic);root.moveBefore(atomic,root.firstChild)},
 ()=>{const group=document.createElementNS(ns,'g');root.append(group);group.moveBefore(atomic,null)},
];
let last=-1;
async function poll(){
 try{
  const {step}=await(await fetch('/command',{cache:'no-store'})).json();
  if(step!==last){
   last=step;actions[step]();
   const element=document.createElementNS(ns,'title');
   let illegalConstructor=false;
   try{new SVGTitleElement()}catch(e){illegalConstructor=e instanceof TypeError}
   await fetch('/report',{method:'POST',body:JSON.stringify({step,documentURL:location.href,title:document.title,
    interface:typeof SVGTitleElement==='function'&&element instanceof SVGTitleElement,
    tag:Object.prototype.toString.call(element),illegalConstructor})});
  }
 }catch(e){await fetch('/report',{method:'POST',body:JSON.stringify({step:last,documentURL:location.href,error:String(e)})})}
 setTimeout(poll,40);
}
poll();
