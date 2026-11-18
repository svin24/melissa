#!/usr/bin/env python3
"""Type text into a QEMU VM via the monitor's sendkey. usage: vmtype.py <monitor.sock> <text> [--enter]"""
import socket, sys, time
sock, text = sys.argv[1], sys.argv[2]
names = {' ':'spc','-':'minus','=':'equal','.':'dot',',':'comma','/':'slash',';':'semicolon',"'":'apostrophe',
         '[':'bracket_left',']':'bracket_right','\\':'backslash','`':'grave_accent','\n':'ret'}
shifted = {'_':'minus','+':'equal','@':'2','!':'1','#':'3','$':'4','%':'5','^':'6','&':'7','*':'8','(':'9',')':'0',
           ':':'semicolon','"':'apostrophe','<':'comma','>':'dot','?':'slash','{':'bracket_left','}':'bracket_right',
           '|':'backslash','~':'grave_accent'}
s = socket.socket(socket.AF_UNIX); s.connect(sock); time.sleep(0.2); s.recv(65536)
def key(k):
    s.sendall(f"sendkey {k}\n".encode()); time.sleep(0.03)
    try: s.setblocking(False); s.recv(65536)
    except BlockingIOError: pass
    finally: s.setblocking(True)
for ch in text:
    if ch.isalpha() and ch.isupper(): key('shift-'+ch.lower())
    elif ch.isalnum(): key(ch)
    elif ch in shifted: key('shift-'+shifted[ch])
    elif ch in names: key(names[ch])
    else: raise SystemExit(f"no key for {ch!r}")
if '--enter' in sys.argv: key('ret')
