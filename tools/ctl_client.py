# -*- coding: utf-8 -*-
"""ctl_client.py —— ListenCtl 管道客户端（发命令、打印一行应答）

用法：
    python tools\\ctl_client.py STATUS
    python tools\\ctl_client.py CHAIN
    python tools\\ctl_client.py SUSPEND          # 模拟“输入法不在前台”
    python tools\\ctl_client.py RESUME
    python tools\\ctl_client.py REPAIR           # 等于设置页那个「一键修复」按钮
    python tools\\ctl_client.py SET voice off    # SET 支持：backend/polish/hotkey_mode/voice
    python tools\\ctl_client.py QUIT

为什么需要它：ListenCtl 原本只有 fork（C++）和 `listen.exe --status` 两个客户端；
排查/回归时要能手动发 SUSPEND/RESUME/REPAIR（例如验证「挂起/释放后按 F9 能不能把链路拉回来」）。

⚠️ 必须用 `CreateFileW`（ctypes）而不是 Python 的 `open()`：
服务端建管道时不给共享模式，`open()` 会被直接拒掉（实测返回 None）。
"""
import ctypes
import sys
import time
from ctypes import wintypes

PIPE = r'\\.\pipe\ListenCtl'
GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
OPEN_EXISTING = 3
INVALID = ctypes.c_void_p(-1).value

_k32 = ctypes.WinDLL('kernel32', use_last_error=True)
_k32.CreateFileW.restype = wintypes.HANDLE
_k32.CreateFileW.argtypes = [
    wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
    wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
]
_k32.WaitNamedPipeW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD]
_k32.WriteFile.argtypes = [wintypes.HANDLE, ctypes.c_char_p, wintypes.DWORD,
                           ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
_k32.ReadFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                          ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
_k32.PeekNamedPipe.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                               ctypes.POINTER(wintypes.DWORD),
                               ctypes.POINTER(wintypes.DWORD),
                               ctypes.POINTER(wintypes.DWORD)]
_k32.CloseHandle.argtypes = [wintypes.HANDLE]


def _connect(wait_ms=800):
    h = _k32.CreateFileW(PIPE, GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_EXISTING, 0, None)
    if h == INVALID:
        # 服务端可能正忙（也可能是没在跑）
        if not _k32.WaitNamedPipeW(PIPE, wait_ms):
            return None
        h = _k32.CreateFileW(PIPE, GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_EXISTING, 0, None)
        if h == INVALID:
            return None
    return h


def query(cmd, timeout_s=1.0):
    """发一条命令，返回应答行（去掉换行）；None = listen 不在跑/超时"""
    h = _connect()
    if h is None:
        return None
    try:
        buf = (cmd + '\n').encode('utf-8')
        wrote = wintypes.DWORD(0)
        if not _k32.WriteFile(h, buf, len(buf), ctypes.byref(wrote), None):
            return None
        out = b''
        start = time.time()
        while b'\n' not in out:
            avail = wintypes.DWORD(0)
            if not _k32.PeekNamedPipe(h, None, 0, None, ctypes.byref(avail), None):
                return None
            if avail.value:
                chunk = ctypes.create_string_buffer(avail.value)
                got = wintypes.DWORD(0)
                if not _k32.ReadFile(h, chunk, avail.value, ctypes.byref(got), None):
                    return None
                out += chunk.raw[:got.value]
                continue
            if time.time() - start > timeout_s:
                return None
            time.sleep(0.005)
        return out.split(b'\n')[0].decode('utf-8', 'replace')
    finally:
        _k32.CloseHandle(h)


if __name__ == '__main__':
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    r = query(' '.join(sys.argv[1:]))
    print('(listen 未运行 / 无应答)' if r is None else r)
    sys.exit(0 if r else 1)
