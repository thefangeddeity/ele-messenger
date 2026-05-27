#!/usr/bin/env python3
# ELE Messenger - eleadmin v0.1.0
import curses, json, os, subprocess, requests
from datetime import datetime

BASE = os.path.dirname(os.path.abspath(__file__))
CFG = os.path.join(BASE, 'config.json')
UVI = os.path.join(BASE, '.venv/bin/uvicorn')
proc = None

def cfg(): return json.load(open(CFG)) if os.path.exists(CFG) else {}
def savecfg(c): json.dump(c, open(CFG,'w'), indent=2)
def get(p):
    try: return requests.get(f'http://127.0.0.1:8000{p}', timeout=1).json()
    except: return None
def up(): return get('/online') is not None
def start():
    global proc
    c = cfg(); proc = subprocess.Popen([UVI,'server:app','--host',c.get('host','0.0.0.0'),'--port',str(c.get('port',8000))], cwd=BASE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
def stop():
    global proc
    if proc: proc.terminate(); proc = None
    else: subprocess.run(['pkill','-f','uvicorn server:app'], capture_output=True)

def main(s):
    curses.curs_set(0); curses.start_color(); curses.use_default_colors()
    curses.init_pair(1,curses.COLOR_WHITE,-1); curses.init_pair(2,curses.COLOR_GREEN,-1)
    curses.init_pair(3,curses.COLOR_CYAN,-1); curses.init_pair(4,curses.COLOR_RED,-1)
    curses.init_pair(5,curses.COLOR_YELLOW,-1)
    s.timeout(1000); em=None; eb=''; cf=None
    FIELDS=['server_name','tailscale_ip','lan_ip','host','port']
    while True:
        s.erase(); h,w=s.getmaxyx(); c=cfg(); sv=up()
        od=get('/online') or {'online':[]};  ld=get('/api/log') or {'log':[]}
        usr=od.get('online',[]); log=ld.get('log',[])
        t=' ELE Messenger Admin '
        s.addstr(0,max(0,w//2-len(t)//2),t,curses.color_pair(1)|curses.A_BOLD|curses.A_REVERSE)
        sw=max(10,w//2-1)
        try:
            b=curses.newwin(7,sw,1,0); b.attron(curses.color_pair(3)); b.border(); b.attroff(curses.color_pair(3))
            b.addstr(0,2,' SERVER ',curses.color_pair(1)|curses.A_BOLD)
            b.addstr(1,2,'UP' if sv else 'DOWN',(curses.color_pair(2) if sv else curses.color_pair(4))|curses.A_BOLD)
            b.addstr(2,2,f"Host:  {c.get('host','?')}:{c.get('port','?')}"[:sw-4],curses.color_pair(1))
            b.addstr(3,2,f"LAN:   {c.get('lan_ip','?')}"[:sw-4],curses.color_pair(1))
            b.addstr(4,2,f"TS:    {c.get('tailscale_ip','?')}"[:sw-4],curses.color_pair(1))
            b.addstr(5,2,f"Name:  {c.get('server_name','?')}"[:sw-4],curses.color_pair(1))
            b.noutrefresh()
        except curses.error: pass
        try:
            uw=max(10,w-sw-1); ub=curses.newwin(7,uw,1,sw+1)
            ub.attron(curses.color_pair(3)); ub.border(); ub.attroff(curses.color_pair(3))
            ub.addstr(0,2,' ONLINE ',curses.color_pair(1)|curses.A_BOLD)
            [ub.addstr(i+1,2,f'  {u}'[:uw-4],curses.color_pair(2)) for i,u in enumerate(usr[:4])]
            if not usr: ub.addstr(1,2,'(nobody)',curses.color_pair(5))
            ub.addstr(5,2,f'{len(usr)} online',curses.color_pair(1)); ub.noutrefresh()
        except curses.error: pass
        try:
            cb=curses.newwin(len(FIELDS)+2,w,8,0)
            cb.attron(curses.color_pair(3)); cb.border(); cb.attroff(curses.color_pair(3))
            cb.addstr(0,2,' CONFIG  [e] edit ',curses.color_pair(1)|curses.A_BOLD)
            for i,f in enumerate(FIELDS):
                v=str(c.get(f,''))
                if em==f: cb.addstr(i+1,2,f'{f:<15} {eb}_'[:w-4],curses.color_pair(5)|curses.A_BOLD)
                else: cb.addstr(i+1,2,f'{f:<15} {v}'[:w-4],curses.color_pair(1))
            cb.noutrefresh()
        except curses.error: pass
        lt=8+len(FIELDS)+2; lh=h-lt-1
        if lh>2:
            try:
                lb=curses.newwin(lh,w,lt,0)
                lb.attron(curses.color_pair(3)); lb.border(); lb.attroff(curses.color_pair(3))
                lb.addstr(0,2,' EVENT LOG ',curses.color_pair(1)|curses.A_BOLD)
                for i,e in enumerate(log[-(lh-2):]):
                    lb.addstr(i+1,2,f"{e.get('time','')[-8:]}  {e.get('event','')}"[:w-4],curses.color_pair(1))
                lb.noutrefresh()
            except curses.error: pass
        try:
            if cf: s.addstr(h-1,0,f' {cf}  [y] yes  [any] cancel '[:w-1],curses.color_pair(4)|curses.A_BOLD)
            elif em: s.addstr(h-1,0,f' Editing {em}  [Enter] save  [Esc] cancel '[:w-1],curses.color_pair(5))
            else: s.addstr(h-1,0,f' [s] {"stop" if sv else "start"}  [e] edit  [q] quit '[:w-1],curses.color_pair(3))
        except curses.error: pass
        curses.doupdate(); k=s.getch()
        if k==-1: continue
        if cf:
            if k==ord('y'):
                if 'STOP' in cf: stop()
                elif 'START' in cf: start()
            cf=None; continue
        if em:
            if k==27: em=None; eb=''
            elif k in(10,13): c[em]=eb; savecfg(c); em=None; eb=''
            elif k in(curses.KEY_BACKSPACE,127): eb=eb[:-1]
            elif 32<=k<127: eb+=chr(k)
            continue
        if k==ord('q'): break
        elif k==ord('s'): cf='STOP server?' if sv else 'START server?'
        elif k==ord('e'): em=FIELDS[0]; eb=str(cfg().get(FIELDS[0],''))

curses.wrapper(main)
