# probe_listenctl.py — task2 S1/S2 诊断探针
# 直接向运行中的 WeaselServer 注入 IPC 消息（模拟 TSF 上报），
# 用于隔离验证「服务器侧状态机 + 日志 + 拉起逻辑」是否工作。
# 用法: python tools\probe_listenctl.py [tid]
import struct
import sys
import time

PIPE = r'\\.\pipe\kevin-3080\WeaselNamedPipe'
WM_APP = 0x8000
FOCUS_IN = WM_APP + 6
FOCUS_OUT = WM_APP + 7
THREAD_ACTIVATED = WM_APP + 17
TID = int(sys.argv[1]) if len(sys.argv) > 1 else 12345678


def send(f, msg, w, l):
    f.write(struct.pack('<III', msg, w, l))
    f.flush()
    r = f.read(4)
    return struct.unpack('<I', r)[0] if len(r) == 4 else None


f = open(PIPE, 'r+b', buffering=0)
print('connected to', PIPE)
print('FOCUS_IN(%d)            -> %s' % (TID, send(f, FOCUS_IN, TID, 0)))
print('THREAD_ACTIVATED(%d, 1) -> %s' % (TID, send(f, THREAD_ACTIVATED, TID, 1)))
time.sleep(4)  # wait for the 2s debounce + log write
print('THREAD_ACTIVATED(%d, 0) -> %s' % (TID, send(f, THREAD_ACTIVATED, TID, 0)))
time.sleep(4)
print('FOCUS_OUT(%d)           -> %s' % (TID, send(f, FOCUS_OUT, TID, 0)))
f.close()
print('done')
