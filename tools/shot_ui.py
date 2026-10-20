#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""shot_ui.py —— 两个窗口各截一张图（设置窗 + 透明悬浮窗），验证渲染后端改动没画坏

为什么：改 `gui_render`（独显 → WARP 软件光栅）需要肉眼确认字体/布局/半透明没退化。
设置窗在聚焦时按 F9 不响应（Windows 对"输入目标是钩子进程自己的窗口"跳过钩子），
所以分两次跑：一次截设置窗，一次（不聚焦任何本程序窗口）用合成 F9 让悬浮窗出现再截。

用法:
    python tools\\shot_ui.py                  # 用默认 exe（target\\release）
    python tools\\shot_ui.py --tag warp       # 输出名带后缀，便于改前改后对照
"""
import argparse
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_hotkey_suspend as e2e  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(REPO, "target", "release", "listen.exe")
SHOT = os.path.join(REPO, "tools", "shot_win.ps1")
OUTDIR = os.path.join(REPO, ".cache")
SET_TITLE = "倾听输入法 设置"
FLOAT_TITLE = "倾听输入法 悬浮窗"


def kill_listen():
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Stop-Process -Name listen -Force -ErrorAction SilentlyContinue"],
                   capture_output=True)
    time.sleep(1)


def shot(title, name):
    out = os.path.join(OUTDIR, "shot_%s.png" % name)
    r = subprocess.run(["powershell", "-NoProfile", "-File", SHOT, "-Title", title,
                        "-Out", out], capture_output=True, text=True)
    print("  " + (r.stdout or r.stderr).strip())
    return out if os.path.isfile(out) else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", default=EXE)
    ap.add_argument("--tag", default="")
    a = ap.parse_args()
    tag = ("_" + a.tag) if a.tag else ""

    # ① 设置窗（--settings 启动即显示）
    kill_listen()
    p = subprocess.Popen([a.exe, "--settings"], cwd=os.path.dirname(a.exe),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(9)
    print("[设置窗]")
    shot(SET_TITLE, "settings" + tag)
    p.terminate()
    time.sleep(1)

    # ② 悬浮窗（静默启动 + 合成按住 F9 3.5s → 会话期间悬浮窗出现）
    kill_listen()
    p = subprocess.Popen([a.exe], cwd=os.path.dirname(a.exe),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(9)
    e2e.tap_f9(3.5)
    time.sleep(0.6)  # 会话收尾时悬浮窗仍在（隐藏延迟 30s）
    print("[悬浮窗]")
    shot(FLOAT_TITLE, "float" + tag)
    p.terminate()
    time.sleep(0.5)
    kill_listen()


if __name__ == "__main__":
    main()
