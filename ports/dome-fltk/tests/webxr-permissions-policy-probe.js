// Synthetic document behavior shared by top-level, srcdoc and network frames.
async function policyProbe(glChecks=false) {
 const result={origin:location.origin,checks:[]};
 const capture=async(name,fn)=>{try{result[name]=await fn()}catch(error){result[name]={error:error.name}}};
 const p=document.permissionsPolicy;
 result.policy=p?{same:p===document.permissionsPolicy,features:p.features(),allowed:p.allowedFeatures(),xr:p.allowsFeature('xr-spatial-tracking'),geo:p.allowsFeature('geolocation'),gamepad:p.allowsFeature('gamepad'),unknown:p.allowsFeature('not-a-supported-feature'),invalid:p.allowsFeature('xr-spatial-tracking','not an origin'),allowlist:p.getAllowlistForFeature('xr-spatial-tracking')}:null;
 await capture('gamepads',()=>Array.isArray(navigator.getGamepads()));
 await capture('geolocationPermission',async()=>(await navigator.permissions.query({name:'geolocation'})).state);
 await capture('inline',()=>navigator.xr.isSessionSupported('inline'));
 await capture('vr',()=>navigator.xr.isSessionSupported('immersive-vr'));
 await capture('ar',()=>navigator.xr.isSessionSupported('immersive-ar'));
 await capture('viewer',async()=>{const session=await navigator.xr.requestSession('inline');const features=[...session.enabledFeatures];await session.requestReferenceSpace('viewer');await session.end();return features});
 await capture('local',async()=>{const session=await navigator.xr.requestSession('inline',{requiredFeatures:['local']});const features=[...session.enabledFeatures];await session.requestReferenceSpace('local');await session.end();return features});
 await capture('optional',async()=>{const session=await navigator.xr.requestSession('inline',{optionalFeatures:['local','local-floor','bounded-floor','unbounded']});const features=[...session.enabledFeatures];await session.end();return features});
 if(glChecks){result.gl=[];for(const version of ['webgl','webgl2']){
  const canvas=document.createElement('canvas');canvas.width=4;canvas.height=4;document.body.append(canvas);const gl=canvas.getContext(version,{xrCompatible:true});
  if(!gl){result.gl.push({version,error:'No WebGL context'});continue}
  const initial=gl.getContextAttributes().xrCompatible;let error=null;try{await gl.makeXRCompatible()}catch(e){error=e.name}
  result.gl.push({version,initial,after:gl.getContextAttributes().xrCompatible,error});gl.getExtension('WEBGL_lose_context').loseContext();canvas.remove();
 }}
 return result;
}
