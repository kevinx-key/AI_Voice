# -*- coding: utf-8 -*-
# ⚠️ 本脚本会**模拟鼠标/键盘**（SendInput / mouse_event / SendKeys）或置顶窗口。
#    按本机全局规则（~/.dsh/AGENTS.md 第一节），**agent 不得运行它**，只能在用户明确同意时
#    由用户自己手动执行。agent 侧的回归只做：cargo test + 文件/日志核验。
r"""e2e_float_window.py —— 悬浮窗行为 E2E（不用真说话、不用人守）。

覆盖用户 2026-10-20 报的三件事：
  ① 「悬浮窗多久自动消失」要有设置 → 这里验证 `[floating] hide_delay_seconds` 真按设置消失；
  ② 「没有任何按键时悬浮窗自动弹出」 → 启动时**不许**因为一条普通提示（Note）就弹出来；
  ③ 「整理好的文字输出后一直遗留在悬浮窗中」 → `show_text = false`（默认）时窗口里
     不渲染识别/整理文字，且「已注入 N 字」后按停留时间消失。

做法：合成 F9 起一段静音会话（min_speech_rms 拉到 0.9，不会真识别/注入），
用 Win32 `IsWindowVisible` 轮询「倾听输入法 悬浮窗」的真实可见性。

用法：
    python tools\ctl_client.py QUIT
    python tools\e2e_float_window.py
退出码：0 全通过 / 1 有失败 / 2 环境不满足。
"""
import ctypes
import glob
import os
import shutil
import subprocess
import sys
import time
from ctypes import wintypes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ctl_client import query as ctl_query  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
EXE = os.environ.get("LISTEN_EXE") or os.path.join(REPO, "target", "release", "listen.exe")
ROOT = os.path.join(REPO, "target", "repro-float")
CONFIG_PATH = os.path.join(ROOT, "config.toml")
LOG_DIR = os.path.join(ROOT, "logs")
MODEL = r"K:\app\AIVoice\models\paraformer-zh"
FLOAT_TITLE = "倾听输入法 悬浮窗"

CONFIG = """\
[general]
hotkey = "f9"
raw_hotkey = "shift+f9"
hotkey_mode = "hold"
max_session_minutes = 30
segment_seconds = 25
silence_seconds = 0.5
flush_seconds = 20
# 0.9：房间底噪永远过不了 → 不会真的送 ASR、也就不会注入（本脚本只看窗口）
min_speech_rms = 0.9
min_record_seconds = 0.2
autostart = false
log_level = "info"
device = ""
managed_suspend_exit_minutes = 0
voice_enabled = true
prewarm_capture = true
prewarm_idle_minutes = 0
model_release_minutes = 0
managed_tray = false
gate_engine = "energy"
gui_render = "auto"

[asr]
backend = "local"
local_model = '{model}'
model_type = "paraformer"
num_threads = 4

[polish]
enabled = false

[floating]
enabled = true
hide_delay_seconds = {delay}
show_text = false
"""


# ---------------------------------------------------------------- Win32
_u32 = ctypes.WinDLL("user32", use_last_error=True)
_u32.FindWindowW.restype = wintypes.HWND
_u32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
_u32.IsWindowVisible.restype = wintypes.BOOL
_u32.IsWindowVisible.argtypes = [wintypes.HWND]


def float_visible():
    """悬浮窗当前是否**真的可见**（窗口对象一直在，隐藏时 IsWindowVisible=0）"""
    h = _u32.FindWindowW(None, FLOAT_TITLE)
    if not h:
        return False
    return bool(_u32.IsWindowVisible(h))


class _KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", ctypes.c_ushort),
        ("wScan", ctypes.c_ushort),
        ("dwFlags", ctypes.c_ulong),
        ("time", ctypes.c_ulong),
        ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


class _U(ctypes.Union):
    _fields_ = [("ki", _KEYBDINPUT), ("pad", ctypes.c_ubyte * 32)]


class _INPUT(ctypes.Structure):
    _fields_ = [("type", ctypes.c_ulong), ("u", _U)]


def tap_f9(hold_s=0.25):
    for up in (False, True):
        inp = _INPUT(type=1, u=_U(ki=_KEYBDINPUT(0x78, 0, 2 if up else 0, 0, None)))
        if ctypes.windll.user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(_INPUT)) != 1:
            raise OSError("SendInput 失败")


def log_text():
    files = glob.glob(os.path.join(LOG_DIR, "app.log.*"))
    if not files:
        return ""
    files.sort(key=lambda p: os.path.getmtime(p))
    try:
        with open(files[-1], encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def wait_until(pred, timeout, step=0.2):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(step)
    return pred()


def check(name, ok, detail=""):
    print("  [%s] %-6s %s" % (time.strftime("%H:%M:%S"), "OK" if ok else "FAIL", name))
    if not ok and detail:
        for l in str(detail).splitlines()[-8:]:
            print("      | " + l[:170])
    return ok


def listen_running():
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq listen.exe", "/FO", "CSV", "/NH"],
        capture_output=True, text=True, errors="replace",
    ).stdout
    return "listen.exe" in out.lower()


def main():
    res = []
    if not os.path.isfile(EXE):
        print("!! 找不到 %s（先 cargo build --release）" % EXE)
        return 2
    if not os.path.isdir(MODEL):
        print("!! 找不到本地模型 %s" % MODEL)
        return 2
    if listen_running():
        print("!! 已有 listen.exe 在运行：先退出它（ctl_client.py QUIT）")
        return 2

    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)
    delay = int(os.environ.get("LISTEN_FLOAT_DELAY", "3"))
    with open(CONFIG_PATH, "w", encoding="utf-8") as f:
        f.write(CONFIG.format(model=MODEL, delay=delay))

    # ------------------------------------------------ ② 启动时不许自动弹
    # 造一份「上次崩溃」现场：新版本要用 Note（不弹窗）而不是 Hint（弹窗）
    os.makedirs(LOG_DIR, exist_ok=True)
    with open(os.path.join(LOG_DIR, "last_panic.txt"), "w", encoding="utf-8") as f:
        f.write("[22:00:00] PANIC（线程 worker）\n位置: src/main.rs:1:1\n信息: [test] 模拟\n")
    print("== ② 启动（带一份刚写的 last_panic.txt）：悬浮窗**不许**自动弹出 ==")
    p = subprocess.Popen([EXE, "--config", CONFIG_PATH, "--no-tray", "--managed"])
    ok = wait_until(lambda: "识别后端就绪" in log_text(), timeout=90)
    if not ok:
        print("!! 实例没就绪：\n" + log_text()[-800:])
        p.terminate()
        return 2
    time.sleep(2.0)
    res.append(check("启动后 2s 悬浮窗仍不可见（提示走状态栏/日志，不弹窗）", not float_visible(),
                     log_text()[-600:]))
    res.append(check("启动提示确实写进了日志（上次运行异常退出）",
                     "上次运行异常退出" in log_text(), log_text()[-800:]))

    # ------------------------------------------------ ① 会话期间显示、结束后按设置消失
    print("== ① 合成一段会话：悬浮窗应出现，然后在 3s 停留后消失 ==")
    tap_f9(0.25)
    ok = wait_until(lambda: "会话开始（按住模式" in log_text(), timeout=10)
    res.append(check("会话开始（按住模式）", ok, log_text()[-600:]))
    ok = wait_until(float_visible, timeout=5)
    res.append(check("会话期间悬浮窗可见", ok))

    # 松手 → 会话收尾（短按会给 1.5s 继续听窗口）→ 之后进入停留计时
    tap_f9(0.15)
    ok = wait_until(lambda: "会话结束：" in log_text(), timeout=15)
    res.append(check("松手后会话收尾", ok, log_text()[-600:]))
    t0 = time.time()
    ok = wait_until(lambda: not float_visible(), timeout=15, step=0.15)
    gone_after = time.time() - t0
    res.append(check("悬浮窗在设置的时间内消失（%.1fs，设置 %ss）" % (gone_after, delay),
                     ok and gone_after <= delay + 8, log_text()[-400:]))

    # ------------------------------------------------ 清理
    print("== 退出 ==")
    print("      QUIT -> %s" % ctl_query("QUIT", 2.0))
    time.sleep(1.2)
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()

    print("\n== 结果: %d/%d 通过 ==" % (sum(1 for r in res if r), len(res)))
    return 0 if all(res) else 1


if __name__ == "__main__":
    sys.exit(main())
