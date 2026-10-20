#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""gui_stall_bench.py —— GUI 定格（帧间隔 >300ms）复现与量化

为什么需要：`WARN listen::gui: GUI 帧间隔 Nms` 是应用自带的看门狗（ui() 两次调用之间的间隔）。
2026-10-08 实测 7 次定格（最高 3361ms），但**成因没定论**：既有 llama-server 拉起/回收的时刻，
也有窗口显示/隐藏的时刻，还有 2 次无关联事件。改渲染参数（present_mode / 适配器）之前，
先要一个「同一套动作 → 同样几次定格」的对照台，否则只能靠体感。

四个阶段（可单选，默认全做）：
    hide     窗口隐藏时静置            —— 复现「隐藏窗口仍 20fps Present」的阻塞
    show     窗口显示时静置
    toggle   反复 显示/隐藏 各 K 次     —— 复现「显示/隐藏瞬间」的定格
    llm      拉起 llama-server(ngl=99) 并持续推理 —— 复现 GPU 争用
    llmstop  杀掉 llama-server          —— 复现「CUDA 上下文销毁」的定格

用法:
    python tools\\gui_stall_bench.py                 # 全跑一遍（约 1.5 分钟 + 模型加载）
    python tools\\gui_stall_bench.py --phases toggle # 只跑某个阶段
    python tools\\gui_stall_bench.py --exe K:\\app\\AIVoice\\listen.exe

⚠️ 跑之前必须没有别的 listen.exe 在运行（单实例互斥 + F9 钩子会互相干扰）。
"""
import argparse
import ctypes
import glob
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEF_EXE = os.path.join(REPO, "target", "release", "listen.exe")
TITLE = "倾听输入法 设置"
LLM = r"K:\projects\llama.cpp\build\bin\Release\llama-server.exe"
LLM_MODEL = r"K:\AI_Voice\models\llm\Qwen3.5-4B-Q4_K_M.gguf"
STALL_RE = re.compile(r"GUI 帧间隔 (\d+)ms")

u32 = ctypes.windll.user32


def log_path(exe):
    d = os.path.join(os.path.dirname(exe), "logs")
    fs = glob.glob(os.path.join(d, "app.log.*"))
    if not fs:
        sys.exit("找不到 %s\\app.log.* —— exe 路径对吗？" % d)
    return max(fs, key=os.path.getmtime)


def hwnd():
    h = u32.FindWindowW(None, TITLE)
    return h


def visible():
    h = hwnd()
    return bool(h) and bool(u32.IsWindowVisible(h))


def hide_window():
    h = hwnd()
    if h:
        u32.PostMessageW(h, 0x0010, 0, 0)  # WM_CLOSE → 应用自己处理成「隐藏到托盘」


def show_window(exe):
    subprocess.run([exe, "--settings"], capture_output=True, timeout=30)


def stalls_since(logf, start_ts):
    """读日志里 start_ts 之后的定格告警 → [ms,…]"""
    out = []
    for line in open(logf, encoding="utf-8", errors="replace"):
        if not line[:4].isdigit():
            continue
        if line[:19] < start_ts:
            continue
        if m := STALL_RE.search(line):
            out.append(int(m.group(1)))
    return out


def llm_load(seconds, n_predict=400):
    """持续向 llama-server 要求长生成（把 GPU 占满）"""
    body = json.dumps({"prompt": "请把下面这段话改写得更正式一些：" + "今天天气不错。" * 20,
                       "n_predict": n_predict, "stream": False, "temperature": 0.2}).encode()
    req = urllib.request.Request("http://127.0.0.1:18080/completion", data=body,
                                 headers={"Content-Type": "application/json"})
    stop = time.time() + seconds
    n = 0
    while time.time() < stop:
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                r.read()
            n += 1
        except Exception:
            time.sleep(0.5)
    return n


def wait_llm_up(timeout=180):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            with urllib.request.urlopen("http://127.0.0.1:18080/health", timeout=3):
                return True
        except Exception:
            time.sleep(2)
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", default=DEF_EXE)
    ap.add_argument("--phases", default="hide,show,toggle,llm,llmstop")
    ap.add_argument("--secs", type=int, default=15, help="每个静止阶段的秒数")
    ap.add_argument("--toggles", type=int, default=8, help="toggle 阶段循环次数")
    a = ap.parse_args()
    phases = [p.strip() for p in a.phases.split(",") if p.strip()]

    others = subprocess.run(["powershell", "-NoProfile", "-Command",
                             "(Get-Process listen -ErrorAction SilentlyContinue).Count"],
                            capture_output=True, text=True).stdout.strip()
    if others not in ("", "0"):
        sys.exit("已有 %s 个 listen.exe 在跑 —— 先停掉（Stop-Process -Name listen -Force）" % others)

    app = subprocess.Popen([a.exe], cwd=os.path.dirname(a.exe),
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(6)  # 等 GUI/渲染起来
    logf = log_path(a.exe)
    results = {}

    def phase(name, fn):
        t0 = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime())
        fn()
        time.sleep(0.5)
        s = stalls_since(logf, t0)
        results[name] = s
        print("  %-8s 定格 %d 次 %s" % (name, len(s), ("最大 %dms" % max(s)) if s else ""))

    def p_hide():
        hide_window()
        time.sleep(a.secs)

    def p_show():
        show_window(a.exe)
        time.sleep(a.secs)

    def p_toggle():
        for _ in range(a.toggles):
            hide_window()
            time.sleep(1.0)
            show_window(a.exe)
            time.sleep(1.0)

    llm_proc = {}

    def p_llm():
        llm_proc["p"] = subprocess.Popen([LLM, "-m", LLM_MODEL, "--host", "127.0.0.1",
                                          "--port", "18080", "-ngl", "99", "-c", "4096"],
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        print("     等 llama-server 起来…")
        if not wait_llm_up():
            print("     ⚠️ 18080 没起来，跳过 llm 阶段")
            return
        n = llm_load(a.secs)
        print("     持续推理 %ds（%d 次请求）" % (a.secs, n))

    def p_llmstop():
        p = llm_proc.get("p")
        if p:
            p.terminate()
            p.wait(timeout=60)
        time.sleep(6)

    print("=" * 78)
    print("GUI 定格基准：exe=%s" % a.exe)
    print("日志 %s" % logf)
    print("=" * 78)
    for ph in phases:
        fn = {"hide": p_hide, "show": p_show, "toggle": p_toggle,
              "llm": p_llm, "llmstop": p_llmstop}.get(ph)
        if not fn:
            sys.exit("未知阶段 %s" % ph)
        print("[%s]" % ph)
        phase(ph, fn)

    hide_window()
    time.sleep(1)
    app.terminate()
    time.sleep(1)
    print("-" * 78)
    tot = sum(len(v) for v in results.values())
    worst = max((max(v) for v in results.values() if v), default=0)
    print("合计定格 %d 次，最大 %dms" % (tot, worst))
    for k, v in results.items():
        print("  %-8s %s" % (k, v if v else "无"))
    print("（把上面的列表与改动前对比：同一 exe 同一动作序列应可复现）")


if __name__ == "__main__":
    main()
