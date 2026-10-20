# probe_listen.py — ListenCtl 客户端探针（task2 S6 验证用）
# 用法: python tools\probe_listen.py [命令...]   默认: PING STATUS
# 例:   python tools\probe_listen.py PING STATUS
#       python tools\probe_listen.py SUSPEND STATUS
import sys

PIPE = r'\\.\pipe\ListenCtl'
cmds = sys.argv[1:] or ['PING', 'STATUS']


def read_line(f):
    buf = b''
    while True:
        ch = f.read(1)
        if not ch or ch == b'\n':
            break
        buf += ch
    return buf.decode('utf-8', 'replace')


try:
    f = open(PIPE, 'r+b', buffering=0)
except OSError as e:
    print('connect failed: %s (listen 未运行?)' % e)
    sys.exit(2)

for c in cmds:
    f.write((c + '\n').encode('utf-8'))
    f.flush()
    print('%s -> %s' % (c, read_line(f)))
f.close()
