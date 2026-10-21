#!/usr/bin/env python3
"""Real local-shell -> native Vim -> shell round trip on an isolated X11 display.

Run with xvfb-run. Requires xdotool, ImageMagick import, Pillow and Vim 9.
The test uses isolated XDG paths and a shell without user startup files.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import time

from PIL import Image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    for name in ('config', 'state', 'data', 'cache', 'runtime', 'project'):
        (out / name).mkdir(mode=0o700)
    project = out / 'project'
    first, second = project / 'first source.txt', project / 'second source.txt'
    first.write_text(''.join(f'NATIVE_SCROLL_LINE_{i:03d} unicode e\u0301\n' for i in range(1, 301)))
    second.write_text('second buffer\n')
    shell = out / 'fixture-shell'
    shell.write_text('#!/bin/sh\nexec /bin/bash --noprofile --norc -i\n')
    shell.chmod(0o700)
    config = out / 'config' / 'ember'
    config.mkdir()
    (config / 'config.toml').write_text(
        'font_size = 20.0\ninitial_width = 1400.0\ninitial_height = 950.0\n'
        'terminal_mode = "normal"\nrestore_session = false\n'
        'app_renderer = "wgpu"\nui_font_size = 14.0\njsh_update_check = "never"\n'
    )
    state_file = out / 'vim-state.json'
    quoted = lambda value: "'" + str(value).replace("'", "''") + "'"
    vimrc = out / 'fixture.vim'
    vimrc.write_text(
        'set nocompatible viminfofile=NONE noswapfile nomore notermguicolors\n'
        'set runtimepath=$VIMRUNTIME packpath= noloadplugins\n'
        'set nowrap mouse=a laststatus=0 nonumber norelativenumber nospell nolist conceallevel=0\n'
        'syntax on\n'
        'highlight Normal guifg=#ccddee guibg=#101820 ctermfg=15 ctermbg=1\n'
        'highlight NativeRgb guifg=#19c3a7 guibg=#112233 ctermfg=196 ctermbg=21\n'
        'augroup shell_native_fixture\nautocmd!\n'
        'autocmd BufEnter * syntax match NativeRgb /^NATIVE_SCROLL.*/\naugroup END\n'
        'function! FixtureState(timer) abort\n'
        "let scripts = getscriptinfo({'name': 'vim9-ide-bridge.vim$'})\n"
        "let native = !empty(scripts)\n"
        f"call writefile([json_encode({{'pid': getpid(), 'cwd': getcwd(), 'args': argv(), 'line': line('.'), 'topline': winsaveview().topline, 'native': native, 'text': getline('.'), 'window': win_getid(), 'mode': mode(), 'views': getwininfo()}})], {quoted(str(state_file)+'.tmp')})\n"
        f"call rename({quoted(str(state_file)+'.tmp')}, {quoted(state_file)})\n"
        "endfunction\ncall timer_start(40, function('FixtureState'), {'repeat': -1})\n"
    )
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('JTERM_VIM9_BIN', None)
    env.pop('GVIMINIT', None)
    env.update({
        'XDG_CONFIG_HOME': str(out/'config'), 'XDG_STATE_HOME': str(out/'state'),
        'XDG_DATA_HOME': str(out/'data'), 'XDG_CACHE_HOME': str(out/'cache'),
        'XDG_RUNTIME_DIR': str(out/'runtime'), 'EMBER_SHELL': str(shell),
        'VIMINIT': "execute 'source ' . fnameescape("+quoted(vimrc)+")",
        'WGPU_BACKEND': 'gl', 'LIBGL_ALWAYS_SOFTWARE': '1', 'WINIT_UNIX_BACKEND': 'x11',
    })
    log = (out/'application.log').open('w')
    app = subprocess.Popen([str(args.binary.resolve())], cwd=project, env=env, stdout=log, stderr=log)
    def xdo(*words):
        r = subprocess.run(['xdotool', *map(str, words)], env=env, capture_output=True, text=True, timeout=5)
        if r.returncode and words[0] != 'search':
            raise RuntimeError(r.stderr)
        return r.stdout.strip()
    def wait(probe, label, timeout=25):
        until=time.monotonic()+timeout
        while time.monotonic()<until:
            value=probe()
            if value:return value
            if app.poll() is not None:raise RuntimeError(f'{label}: Ember exited {app.returncode}')
            time.sleep(.04)
        raise RuntimeError('Timed out: '+label)
    def state():
        try:return json.loads(state_file.read_text())
        except (FileNotFoundError,json.JSONDecodeError):return {}
    def windows():return set(xdo('search','--onlyvisible','--class','ember').splitlines())
    def type_line(text):
        xdo('type','--clearmodifiers','--delay','6',text)
        xdo('key','Return')
    def vim_command(text):
        xdo('key','Escape')
        type_line(':'+text)
    report={}
    try:
        initial=wait(windows,'shell window')
        assert len(initial)==1,initial
        root=next(iter(initial))
        xdo('windowfocus','--sync',root)
        status=project/'return-status.txt'
        status_command="; printf '%s\\n' \"$?\" > "+shlex.quote(str(status))
        type_line('vim +42 '+shlex.quote(first.name)+' '+shlex.quote(second.name)+status_command)
        child=wait(lambda: next(iter(windows()-initial),None),'automatically opened native editor')
        xdo('windowfocus','--sync',child)
        launch=wait(lambda: state() if state().get('native') and state().get('line')==42 else None,'native Vim received +42')
        assert launch['cwd']==str(project),launch
        assert launch['args']==[first.name,second.name],launch
        assert not status.exists(),'the source shell must wait while the editor is alive'
        report['shell_argv_and_cwd']=launch
        image=out/'native-shell-entry.png'
        def native_pixels():
            subprocess.run(['import','-window',child,str(image)],env=env,check=True,timeout=5)
            im=Image.open(image).convert('RGB')
            return sum(count for count,color in (im.getcolors(im.width*im.height) or []) if color==(0x19,0xc3,0xa7))
        report['native_rgb_pixels']=wait(lambda: n if (n:=native_pixels())>100 else None,'native RGB frame')
        # Startup resizes the initial PTY to the actual native content rect.
        # Let that acknowledgement settle before measuring a wheel delta.
        time.sleep(.3)
        xdo('mousemove','--window',child,650,320)
        before=state()['topline']
        report['wheel_before']=before
        # This cached winit X11 backend reports both XTest button edges as
        # separate wheel events. Measure one event first, then both edges;
        # never halve production deltas or discard real repeated wheel input.
        xdo('mousedown',5)
        wait(lambda: state().get('topline')==before+3,'one wheel event = three lines')
        xdo('mouseup',5)
        wait(lambda: state().get('topline')==before+6,'XTest release is another wheel event')
        xdo('click','--repeat',5,'--delay',12,5)
        wait(lambda: state().get('topline')==before+36,'coalesced synthetic wheel burst')
        xdo('click',4)
        wait(lambda: state().get('topline')==before+30,'wheel reverses immediately')
        report['wheel_topline']={'before':before,'after':state()['topline'],'net_lines':30,
                                 'lines_per_event':3,'xtest_events_per_click':2}
        vim_command('set splitright | vsplit')
        split=wait(lambda: state() if len(state().get('views',[]))==2 else None,'two Vim splits')
        left=min(split['views'],key=lambda view:view['wincol'])
        assert split['window']!=left['winid'],split
        xdo('type','--clearmodifiers','--delay','15','i')
        focused=wait(lambda: state() if state().get('mode')=='i' else None,'insert mode in right split')
        xdo('mousemove','--window',child,200,320)
        left_top=next(view['topline'] for view in focused['views'] if view['winid']==left['winid'])
        def hovered_top():
            return next((view['topline'] for view in state().get('views',[]) if view['winid']==left['winid']),None)
        xdo('mousedown',5)
        wait(lambda: hovered_top()==left_top+3,'hovered inactive split scroll')
        xdo('mouseup',5)
        wait(lambda: hovered_top()==left_top+6,'hovered inactive split synthetic release')
        after=state()
        assert after['window']==focused['window'] and after['mode']=='i',after
        assert after['line']==focused['line'] and after['topline']==focused['topline'],after
        report['hovered_split']={'focused_window':after['window'],'hovered_window':left['winid'],
                                 'mode':after['mode'],'focused_line':after['line'],
                                 'before_topline':left_top,'after_topline':hovered_top()}
        vim_command('only')
        wait(lambda: len(state().get('views',[]))==1,'single editing window restored')
        xdo('type','--clearmodifiers','--delay','15','ggIauto_native_')
        wait(lambda: state().get('text','').startswith('auto_native_'),'native insert')
        xdo('key','Escape')
        xdo('key','ctrl+s')
        wait(lambda: first.read_text().startswith('auto_native_'),'native save')
        vim_command('qa')
        wait(lambda: child not in windows(),'editor exit')
        xdo('windowfocus','--sync',root)
        wait(status.exists,'shell resumed')
        assert status.read_text().strip()=='0',status.read_text()
        report['shell_return_status']=0
        old_pid=state()['pid']
        status.unlink()
        type_line('vim '+shlex.quote(second.name)+status_command)
        child=wait(lambda: next(iter(windows()-initial),None),'second native editor')
        xdo('windowfocus','--sync',child)
        wait(lambda: state().get('native') and state().get('pid')!=old_pid,'second native bridge')
        assert not status.exists(),'the shell must wait for the second editor too'
        vim_command('cq7')
        wait(lambda: child not in windows(),'nonzero editor exit')
        xdo('windowfocus','--sync',root)
        wait(status.exists,'shell resumed after :cq7')
        assert status.read_text().strip()=='7',status.read_text()
        report['shell_nonzero_return_status']=7
        old_pid=state()['pid']
        type_line('JTERM_VIM9_AUTO_GUI=0 vim +3 '+shlex.quote(first.name))
        wait(lambda: state().get('pid')!=old_pid and state().get('line')==3,'terminal opt-out')
        assert not state()['native'],state()
        assert windows()==initial,windows()
        vim_command('qa')
        report['terminal_opt_out']='passed'
        (out/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report,indent=2))
    except Exception:
        report['failed_state']=state()
        (out/'report.json').write_text(json.dumps(report,indent=2))
        raise
    finally:
        if app.poll() is None:
            app.terminate()
            try:app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill();app.wait(timeout=5)
        log.close()

if __name__=='__main__':main()
