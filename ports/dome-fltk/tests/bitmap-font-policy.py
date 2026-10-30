#!/usr/bin/env python3
"""Host bitmap appearance, URL-font policy and document decoding regression."""
import argparse,json,pathlib,subprocess,sys
P=pathlib.Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',type=P,required=True);p.add_argument('--output',type=P,required=True)
a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
runs=[]
for encoding,text,scale in [('utf-8','Příliš žluťoučký kůň 日本語 한국어 العربية',1),('windows-1250','Příliš žluťoučký kůň',1),('windows-1251','Привет мир',1),('iso-8859-1','Crème brûlée',1),('koi8-r','Привет мир',2)]:
    folder=out/encoding;folder.mkdir(exist_ok=True)
    appearance=folder/'appearance.json';appearance.write_text(json.dumps(dict(font='Unifont / Regular',font_size=16,font_scale=scale)))
    # JSON escapes keep JS source ASCII while the document text exercises decoding.
    fixture=folder/'fixture.html'
    html='''<!doctype html><style>@font-face{font-family:PageOutline;src:url('/font-api.ttf')}body{font:19px PageOutline}#probe{font:72px PageOutline,serif}</style><p id="probe">TEXT</p><script>
(async()=>{await document.fonts.ready;let checks=[];function check(passed,name){checks.push({passed:!!passed,name})}
const probe=document.querySelector('#probe'),style=getComputedStyle(probe);
check(style.fontFamily.includes('Dome Browser Bitmap '),'Exact host bitmap family overrides page fonts');
check(getComputedStyle(document.body).fontSize==='PIXELSpx','Host strike pixel size and integer scale');
check(probe.textContent===EXPECTED,'Declared codepage decodes to Unicode');
check(probe.getBoundingClientRect().height>0,'Native bitmap text produces layout');
let cfg=await fetch('/config.json').then(r=>r.json());await fetch('/report',{method:'POST',body:JSON.stringify({phase:cfg.phase,checks})})})().catch(async e=>{await fetch('/report',{method:'POST',body:JSON.stringify({checks:[{passed:false,name:String(e)}]})})});
</script>'''.replace('TEXT',text).replace('PIXELS',str(16*scale)).replace('EXPECTED',json.dumps(text,ensure_ascii=True))
    fixture.write_bytes(html.encode(encoding))
    subprocess.run([sys.executable,str(P(__file__).with_name('cache-api.py')),'--binary',str(a.binary.resolve()),'--fixture',str(fixture),'--appearance-fixture',str(appearance),'--fixture-encoding',encoding,'--stages','write','--capture-frame','--timeout','30','--output',str(folder/'run')],check=True)
    report=json.loads((folder/'run/report.json').read_text());assert not any('/font-api.ttf' in r['path'] for r in report['requests']),'Page font fetched'
    assert all(r['clean_close'] for r in report['runs'])
    runs.append(dict(encoding=encoding,scale=scale,passed=True,binary_sha256=report['binary_sha256']))
(out/'report.json').write_text(json.dumps(dict(passed=True,runs=runs),indent=2)+'\n')
print('PASS host bitmap family/size, 1x/2x, five codepages, no URL fonts, clean shutdown')
