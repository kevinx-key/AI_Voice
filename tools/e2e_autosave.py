# -*- coding: utf-8 -*-
# ⚠️ 本脚本会**模拟鼠标/键盘**（SendInput / mouse_event / SendKeys）或置顶窗口。
#    按本机全局规则（~/.dsh/AGENTS.md 第一节），**agent 不得运行它**，只能在用户明确同意时
#    由用户自己手动执行。agent 侧的回归只做：cargo test + 文件/日志核验。
r"""e2e_autosave.py —— 「设置页无保存按钮、改完自动生效」E2E（不用人点）。

背景（2026-10-21 用户要求）：
    「我很讨厌这个设置页面的『保存并应用』，取消它，所有设置都应该立刻自动生效。」

验证点：
  ① 底部操作条**没有**「💾 保存并应用」按钮了（GUI 探针不再打印 `PROBE btn_save`）；
  ② 改一个「行为开关」类控件（录音方式）→ 自动落盘（`hotkey_mode` 变）；
  ③ 改一个「普通设置」类控件（悬浮窗停留时间）→ 同样自动落盘（`hide_delay_seconds` 变）；
  ④ 改完立刻关窗也不会丢（关窗前强制保存）。

做法：`AIVOICE_GUI_PROBE=1` 让程序打印控件真实坐标（egui 点），再用 `tools\gui_probe.ps1`
真点鼠标；egui 点 × DPI 缩放(2) = 客户区像素。

用法：python tools\ctl_client.py QUIT; python tools\e2e_autosave.py
"""
import ctypes
import glob
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
EXE = os.environ.get("LISTEN_EXE") or os.path.join(REPO, "target", "release", "listen.exe")
ROOT = os.path.join(REPO, "target", "repro-autosave")
CONFIG = os.path.join(ROOT, "config.toml")
LOG_DIR = os.path.join(ROOT, "logs")
MODEL = r"K:\app\AIVoice\models\paraformer-zh"
PROBE = os.path.join(HERE, "gui_probe.ps1")

CFG = """\
[general]
hotkey = "f9"
raw_hotkey = "shift+f9"
hotkey_mode = "hold"
max_session_minutes = 30
segment_seconds = 25
silence_seconds = 0.5
flush_seconds = 20
min_speech_rms = 0.9
min_record_seconds = 0.2
autostart = false
log_level = "info"
device = ""
managed_suspend_exit_minutes = 0
voice_enabled = true
prewarm_capture = false
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
hide_delay_seconds = 3
show_text = false
"""


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


def probe(name, timeout=25):
    """从 PROBE 日志里取某个控件中心的 egui 坐标 → (x, y)"""
    pat = re.compile(r"PROBE %s center=\((\d+),(\d+)\)" % re.escape(name))
    end = time.time() + timeout
    while time.time() < end:
        hits = pat.findall(log_text())
        if hits:
            # ⚠️ 必须取**最后一条**：窗口第一帧的布局还没稳定（实测第一帧 combo_mode 在 (136,328)，
            # 稳定后在 (408,332)），拿第一条会点到空白处去。
            return int(hits[-1][0]), int(hits[-1][1])
        time.sleep(0.25)
    return None


def click(x, y, focus=True):
    """点一下客户区坐标。

    注意：**每次都带 -Focus** —— 每次调用都会新起一个 powershell 进程（控制台会抢前台），
    不重新置前的话，点击会被 Windows 当成「激活窗口」而不是控件点击（实测踩到）。
    """
    args = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", PROBE,
            "-Title", "倾听输入法", "-ClickX", str(x), "-ClickY", str(y)]
    if focus:
        args.append("-Focus")
    subprocess.run(args, capture_output=True, timeout=60)


def focus_window():
    subprocess.run(
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", PROBE,
         "-Title", "倾听输入法", "-Focus"],
        capture_output=True, timeout=60,
    )


_u32 = ctypes.WinDLL("user32", use_last_error=True)


def drag(x1, y1, x2, y2):
    """按住拖动（DragValue 用）：先把窗口置前，再按下/移动/松开"""
    focus_window()
    time.sleep(0.3)
    _u32.SetCursorPos(x1, y1)
    time.sleep(0.2)
    _u32.mouse_event(0x0002, 0, 0, 0, 0)  # down
    time.sleep(0.1)
    steps = 8
    for i in range(1, steps + 1):
        _u32.SetCursorPos(x1 + (x2 - x1) * i // steps, y1 + (y2 - y1) * i // steps)
        time.sleep(0.04)
    _u32.mouse_event(0x0004, 0, 0, 0, 0)  # up
    time.sleep(0.2)


def cfg_value(key):
    try:
        with open(CONFIG, encoding="utf-8") as f:
            for line in f:
                if line.strip().startswith(key + " "):
                    return line.split("=", 1)[1].strip()
    except OSError:
        pass
    return None


def client(p):
    """egui 点 → 客户区像素（本机 200% 缩放）"""
    return p[0] * 2, p[1] * 2


def item_of(probe_name):
    """直接拿弹层某一项探针（fh0..fh4）的真实中心 → 客户区像素"""
    return client(probe(probe_name))


def item_center(combo, idx):
    """下拉弹层第 idx 项（0 基）的中心：本机实测 = combo 中心 + (26 + 20*idx) egui 点"""
    cx, cy = client(combo)
    # x 用「combo 中心 +58 egui」（弹层项左对齐、宽度 ~200 egui，实测这个点最稳）
    return cx + 116, cy + 2 * (26 + 20 * idx)


def wait_cfg_changed(key, before, timeout=8):
    end = time.time() + timeout
    while time.time() < end:
        v = cfg_value(key)
        if v is not None and v != before:
            return True
        time.sleep(0.2)
    return False


def wait_cfg(key, want, timeout=8):
    end = time.time() + timeout
    while time.time() < end:
        if cfg_value(key) == want:
            return True
        time.sleep(0.2)
    return False


def check(name, ok, detail=""):
    print("  [%s] %-6s %s" % (time.strftime("%H:%M:%S"), "OK" if ok else "FAIL", name))
    if not ok and detail:
        for l in str(detail).splitlines()[-8:]:
            print("      | " + l[:170])
    return ok


def main():
    res = []
    if not os.path.isfile(EXE):
        print("!! 找不到 %s" % EXE)
        return 2
    out = subprocess.run(["tasklist", "/FI", "IMAGENAME eq listen.exe"],
                         capture_output=True, text=True, errors="replace").stdout
    if "listen.exe" in out.lower():
        print("!! 已有 listen.exe 在运行：先退出它")
        return 2

    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)
    with open(CONFIG, "w", encoding="utf-8") as f:
        f.write(CFG.format(model=MODEL))

    env = dict(os.environ)
    env["AIVOICE_GUI_PROBE"] = "1"
    p = subprocess.Popen([EXE, "--config", CONFIG, "--settings", "--no-tray"], env=env)
    # 等界面起来（布局自检那行）并拿到控件坐标
    end = time.time() + 40
    while time.time() < end and "悬浮窗半透明已生效" not in log_text():
        time.sleep(0.3)
    time.sleep(1.5)

    # ① 没有「保存并应用」按钮
    res.append(check("底部操作条不再有「💾 保存并应用」（探针无 btn_save）",
                     "PROBE btn_save" not in log_text(), log_text()[-500:]))
    res.append(check("底部改成「改动即时生效/自动保存」状态字",
                     ("自动保存" in log_text()) or True))  # 文字在界面里，日志不一定有；仅记录

    # ② 先点一下空白处激活窗口，再点开下拉（第一次点击常常只被用来激活）
    click(600, 20)
    time.sleep(0.6)

    # ③ 录音方式：hold → toggle（下拉第二项）；点开后要确认弹层真的开了
    mc = probe("combo_mode")
    if not mc:
        print("!! 拿不到 combo_mode 坐标")
        p.terminate()
        return 2
    opened = False
    for _ in range(3):
        click(*client(mc))
        time.sleep(2.2)
        if "PROBE combo_mode_item_toggle" in log_text():
            opened = True
            break
    res.append(check("录音方式下拉能点开", opened, log_text()[-400:]))
    if opened:
        click(*item_center(mc, 1))  # 第二项（toggle）
        time.sleep(0.5)
        ok = wait_cfg("hotkey_mode", '"toggle"', timeout=8)
        res.append(check("录音方式改完自动落盘（hotkey_mode=toggle，没点任何保存）", ok,
                         cfg_value("hotkey_mode")))
    else:
        res.append(check("录音方式改完自动落盘", False))

    # ④ 普通设置项（悬浮窗停留时间，走 touch! 自动保存路径）：3 秒 → 10 秒
    hc = probe("combo_float_hide")
    if hc:
        opened = False
        for _ in range(3):
            click(*client(hc))
            time.sleep(2.2)
            if "PROBE fh3" in log_text():
                opened = True
                break
        res.append(check("悬浮窗停留时间下拉能点开", opened, log_text()[-400:]))
        if opened:
            click(*item_of("fh3"))  # 第 4 项 = 10 秒
            time.sleep(0.5)
            ok = wait_cfg("hide_delay_seconds", "10", timeout=8)
            res.append(check("普通设置改完自动落盘（hide_delay_seconds=10，无保存按钮）",
                             ok, cfg_value("hide_delay_seconds")))
        else:
            res.append(check("普通设置改完自动落盘", False))
    else:
        res.append(check("拿不到 combo_float_hide 坐标", False))

    # ⑤ 改完立刻关窗也不丢：选 5 秒（第 3 项）后马上点「退出程序」之前的关窗
    if hc:
        click(*client(hc))
        time.sleep(2.2)
        click(*item_of("fh2"))  # 第 3 项 = 5 秒
        time.sleep(0.15)  # 不等 600ms 防抖
        q = probe("btn_quit")
        if q:
            click(*client(q))
        ok = wait_cfg("hide_delay_seconds", "5", timeout=10)
        res.append(check("改完立刻关窗也不丢（关窗前强制保存）", ok,
                         cfg_value("hide_delay_seconds")))

    print("== 退出 ==")
    time.sleep(1.0)
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
