#!/usr/bin/env python3
"""Capture isolated public Discord/X pages and check browser survival/closure.

Live responses are diagnostics, not deterministic visual-conformance results.
No login or form submission is performed. By default no user profile is used;
--profile-snapshot copies storage from an exclusively inactive profile into a
temporary private test profile without changing the source.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import shutil
import subprocess
import sys
import tempfile
import time
from PIL import Image
from Xlib import X, display, protocol
from Xlib.ext import xtest

signal.pthread_sigmask(signal.SIG_SETMASK,[])
if os.environ.get('DOME_LIVE_PAGES_TEST_BUS')!='1':
    os.execvpe('dbus-run-session',['dbus-run-session','--',sys.executable,*sys.argv],
              {**os.environ,'DOME_LIVE_PAGES_TEST_BUS':'1'})
source=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('urls',nargs='*',default=['https://discord.com/login','https://x.com/'])
p.add_argument('--binary',type=Path,default=source/'target/release/dome-browser')
p.add_argument('--seconds',type=int,default=30)
p.add_argument('--output',type=Path,required=True)
p.add_argument('--trace-shutdown',action='store_true')
p.add_argument('--disable-adblock',action='store_true',help='Disable content filtering only in the temporary test profiles')
p.add_argument('--profile-snapshot',type=Path,help='Read storage from an inactive profile into a disposable private copy')
p.add_argument('--exercise-media',action='store_true',help='Use opt-in layout samples to scroll a video into view and click play on the private X server')
a=p.parse_args();assert 10<=a.seconds<=120
if a.exercise_media:
    assert os.environ.get('DOME_BROWSER_LAYOUT_CAPTURE'),'Media exercise needs layout capture'
    assert not Path(os.environ['DOME_BROWSER_LAYOUT_CAPTURE']).exists(),'Media exercise needs fresh capture output'
out=a.output.resolve();out.mkdir(parents=True,exist_ok=True,mode=0o700)
if a.profile_snapshot:out.chmod(0o700)
binary=a.binary.resolve();runs=[]
for index,url in enumerate(a.urls):
    folder=out/str(index);folder.mkdir(exist_ok=True,mode=0o700)
    record={'url':url,'seconds':a.seconds,'adblock_disabled':a.disable_adblock};app=xvfb=connection=None
    try:
        with tempfile.TemporaryDirectory(prefix='dome-live-page-')as directory:
            runtime=Path(directory);runtime.chmod(0o700)
            if a.profile_snapshot:
                source_profile=a.profile_snapshot.resolve()
                profile=runtime/'profile';profile.mkdir(mode=0o700)
                # Browser startup takes LOCK_EX on this existing file. A shared
                # read lock both rejects a running browser and blocks startup
                # while its persistent stores are being copied.
                with(source_profile/'browser.lock').open('rb')as lock:
                    fcntl.flock(lock,fcntl.LOCK_SH|fcntl.LOCK_NB)
                    for name in ['ck.txt','.ck-initialized','cookie_jar.json','localstorage.json','webstorage','clientstorage','cachestorage']:
                        item=source_profile/name
                        if item.is_dir():shutil.copytree(item,profile/name)
                        elif item.is_file():shutil.copy2(item,profile/name)
                record['disposable_profile_snapshot']=True
            if a.disable_adblock:
                profile=runtime/'profile';profile.mkdir(mode=0o700,exist_ok=True)
                (profile/'adblock-disabled').write_text('disabled for isolated live-page diagnosis\n')
            rd,wr=os.pipe()
            xvfb=subprocess.Popen(['Xvfb','-displayfd',str(wr),'-screen','0','1440x1000x24',
                                   '-nolisten','tcp'],pass_fds=(wr,),stderr=subprocess.DEVNULL)
            os.close(wr)
            with os.fdopen(rd)as stream:screen=':'+stream.readline().strip()
            connection=display.Display(screen)
            env={**os.environ,'DISPLAY':screen,'FLTK_BACKEND':'x11','LIBGL_ALWAYS_SOFTWARE':'1',
                 'XDG_RUNTIME_DIR':directory,'XDG_CONFIG_HOME':directory+'/config',
                 'XDG_DATA_HOME':directory+'/data','XDG_CACHE_HOME':directory+'/cache',
                 'DOME_BROWSER_FRAME_CAPTURE':str(folder/'frame.png')}
            for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET']:env.pop(key,None)
            if a.trace_shutdown:
                env['RUST_LOG']='servo::servo=debug,constellation::constellation=debug'
            with(folder/'browser.log').open('w')as log:
                app=subprocess.Popen([str(binary),'--software','--profile',str(runtime/'profile'),url],
                                     env=env,stdout=log,stderr=log,start_new_session=True)
                started=time.monotonic();snapshots=[];media_actions=[];media_samples=[];seen_media_samples=set()
                def exercise_media():
                    if not a.exercise_media:return
                    base=Path(os.environ['DOME_BROWSER_LAYOUT_CAPTURE'])
                    files=[base,*[base.with_suffix('.'+str(i)+'.json')for i in range(1,24)]]
                    for sample_file in files:
                        if sample_file in seen_media_samples or not sample_file.exists():continue
                        try:data=json.loads(sample_file.read_text())
                        except (OSError,json.JSONDecodeError):continue
                        seen_media_samples.add(sample_file)
                        width,height=data.get('viewport',[0,0]);videos=[v for v in data.get('media',[])if v.get('tag')=='video'and v.get('rect')and v['rect'][2]>0 and v['rect'][3]>0]
                        visible=[v for v in videos if 0<v['rect'][0]+v['rect'][2]/2<width and 0<v['rect'][1]+v['rect'][3]/2<height]
                        media_samples.append({'time':data.get('sampleTime'),'visible':[{'time':v.get('currentTime'),'paused':v.get('paused'),'seeking':v.get('seeking'),'loop':v.get('loop'),'duration':v.get('duration'),'dimensions':v.get('dimensions'),'rect':v.get('rect')}for v in visible]})
                        windows=[w for w in connection.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable and w.get_geometry().width>=800]
                        if not windows or not videos:continue
                        window=windows[0];geometry=window.get_geometry()
                        if visible:
                            video=visible[0]
                            if video.get('paused')and not video.get('errorCode')and len([v for v in media_actions if v['action']=='play-click'])<2:
                                x,y,w,h=video['rect'];px=geometry.x+x+w/2;py=geometry.y+120+y+h/2
                                xtest.fake_input(connection,X.MotionNotify,x=round(px),y=round(py))
                                xtest.fake_input(connection,X.ButtonPress,1);xtest.fake_input(connection,X.ButtonRelease,1);connection.sync()
                                media_actions.append({'action':'play-click','time':data.get('sampleTime'),'rect':video['rect']})
                        elif not any(v['action']=='play-click'for v in media_actions):
                            video=min(videos,key=lambda v:abs(v['rect'][1]-100));steps=max(-80,min(80,round((video['rect'][1]-100)/48)))
                            xtest.fake_input(connection,X.MotionNotify,x=geometry.x+min(600,width//2),y=geometry.y+120+height//2)
                            for _ in range(abs(steps)):
                                button=5 if steps>0 else 4
                                xtest.fake_input(connection,X.ButtonPress,button);xtest.fake_input(connection,X.ButtonRelease,button)
                            connection.sync();media_actions.append({'action':'scroll','time':data.get('sampleTime'),'steps':steps,'rect':video['rect']})
                for interval in [min(10,a.seconds/2),a.seconds]:
                    while app.poll()is None and time.monotonic()-started<interval:
                        exercise_media();time.sleep(.1)
                    titles=[];root=connection.screen().root;size=root.get_geometry()
                    for window in root.query_tree().children:
                        try:
                            title=window.get_wm_name()
                            if title and window.get_attributes().map_state==X.IsViewable:titles.append(title)
                        except Exception:pass
                    raw=root.get_image(0,0,size.width,size.height,X.ZPixmap,0xffffffff)
                    Image.frombytes('RGB',(size.width,size.height),raw.data,'raw','BGRX').save(folder/(str(interval)+'.png'))
                    snapshots.append({'elapsed':round(time.monotonic()-started,2),'titles':titles,'exit_code':app.poll()})
                record.update(snapshots=snapshots,alive_at_end=app.poll()is None)
                if a.exercise_media:
                    previous={};advanced=False
                    for sample in media_samples:
                        for video in sample['visible']:
                            if video.get('paused')is not False or video.get('seeking')is not False or not isinstance(video.get('time'),(int,float)):continue
                            key=(video.get('duration'),tuple(video.get('dimensions')or[]),tuple(round(v)for v in video['rect']))
                            before=previous.get(key);current=video['time']
                            if before is not None and current>.25 and(current>before+.1 or(video.get('loop')and before>current+.1)):advanced=True
                            previous[key]=current
                    record.update(media_actions=media_actions,media_samples=media_samples,
                        media_progress_observed=advanced)
                # Standard close request on this test's private X server.
                for window in root.query_tree().children:
                    try:
                        if window.get_wm_name() and window.get_attributes().map_state==X.IsViewable:
                            event=protocol.event.ClientMessage(window=window,client_type=connection.intern_atom('WM_PROTOCOLS'),
                                data=(32,[connection.intern_atom('WM_DELETE_WINDOW'),X.CurrentTime,0,0,0]))
                            window.send_event(event,event_mask=0)
                    except Exception:pass
                connection.sync()
                try:app.wait(timeout=15);record['clean_close']=app.returncode==0
                except subprocess.TimeoutExpired:
                    record['clean_close']=False
                    threads=[]
                    for task in (Path('/proc')/str(app.pid)/'task').iterdir():
                        try:threads.append({'tid':int(task.name),'name':(task/'comm').read_text().strip(),
                                            'wait_channel':(task/'wchan').read_text().strip()})
                        except OSError:pass
                    (folder/'shutdown-threads.json').write_text(json.dumps(threads,indent=2)+'\n')
                    if a.trace_shutdown:
                        with(folder/'shutdown-stack.log').open('w')as stack:
                            try:
                                trace=subprocess.run(['eu-stack','-p',str(app.pid),'-n','35'],
                                    env={**os.environ,'DEBUGINFOD_URLS':''},stdout=stack,stderr=stack,timeout=20)
                                record['stack_capture_exit_code']=trace.returncode
                            except (OSError,subprocess.TimeoutExpired)as error:
                                record['stack_capture_error']=str(error)
                record['exit_code_after_close']=app.poll()
            lines=(folder/'browser.log').read_text(errors='replace').splitlines()
            record['page_console_errors']=[line for line in lines if line.startswith('PAGE Error ')][:30]
            record['crashes']=[line for line in lines if re.search(r'PAGE_CRASH|panicked at|Segmentation fault|stack overflow',line)][:20]
    except Exception as error:record['harness_error']=str(error)
    finally:
        if app is not None and app.poll()is None:
            os.killpg(app.pid,signal.SIGTERM)
            try:app.wait(timeout=5)
            except subprocess.TimeoutExpired:os.killpg(app.pid,signal.SIGKILL);app.wait()
        if connection is not None:connection.close()
        if xvfb is not None:xvfb.terminate();xvfb.wait(timeout=5)
        runs.append(record)
        scope=('Live response using disposable storage snapshot; source profile unchanged.'if a.profile_snapshot else'Anonymous live response.')
        report={'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'runs':runs,
                'scope':scope+' Isolated profile/bus/X server, software rendering. No whole-site correctness assertion.'}
        (out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps({key:record.get(key)for key in ['url','alive_at_end','clean_close','crashes','harness_error']}),flush=True)
assert all(r.get('alive_at_end')and r.get('clean_close')and not r.get('crashes')and not r.get('harness_error')and(not a.exercise_media or r.get('media_progress_observed'))for r in runs),runs
