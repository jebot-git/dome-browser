async function workerProbe(cfg) {
    let fileAllowed=false, fileError='', positive=false;
    try { await fetch(cfg.localImage,{mode:'no-cors'}); fileAllowed=true; }
    catch(error) { fileError=error.name; }
    try { positive=(await(await fetch(cfg.serverOrigin+'/svg-image.svg?name=worker-positive&cors=anonymous')).text()).includes('<svg'); }
    catch(error) { return {fileAllowed,fileError,positive,error:String(error)}; }
    return {fileAllowed,fileError,positive,origin:self.origin};
}
