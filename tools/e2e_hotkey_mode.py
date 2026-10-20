# -*- coding: utf-8 -*-
# ⚠️ 本脚本会**模拟鼠标/键盘**（SendInput / mouse_event / SendKeys）或置顶窗口。
#    按本机全局规则（~/.dsh/AGENTS.md 第一节），**agent 不得运行它**，只能在用户明确同意时
#    由用户自己手动执行。agent 侧的回归只做：cargo test + 文件/日志核验。
r"""e2e_hotkey_mode.py —— 录音方式语义 E2E（hold / toggle），**不用真说话**。
为什么有它（2026-10-20 用户报的两个 bug）：

  1) 设置里选「切换：按一下开始 / 再按一下停止」，实际仍按「按住说话」跑
     （不管选哪个，都是按下说话、松手截止）；
  2) 按住 F9 说话、**不松手**，程序却自己把会话收尾了，而且很频繁。

这两条都只跟「热键 → 会话生命周期」有关，跟识别质量无关，所以本脚本只用 SendInput
合成 F9，然后把**日志里出现的会话边界**当成断言对象：

  ① toggle：点一下 → 会话必须继续活着（不许出现「会话结束」）；再点一下 → 必须收尾；
  ② toggle 会话总时长必须 ≈ 两次按下之间（不是「松手就截止」）；
  ③ **直接改 config.toml**（不经设置页）→ worker 必须自己发现并热生效；
  ④ hold：按住不松 → 会话必须一直活着；松手 → 必须收尾；
  ⑤ hold：按住时来一对「假 KEYUP+KEYDOWN」（键盘抖动/别的设备注入）→ **不许收尾**；
  ⑥ hold：按住时收到 SUSPEND（输入法切走）→ **不许截断录音**，收尾后才挂起；
  ⑦ hold + 键盘自动重复（~30Hz KEYDOWN）→ 不许被重复事件收尾。

用法：
    # 先退出正在运行的倾听输入法（否则单实例互斥体会让本实例秒退）
    python tools\\ctl_client.py QUIT      # 或从托盘退出
    python tools\\e2e_hotkey_mode.py

    # 也可以指定被测 exe（默认 target\release\listen.exe）
    $env:LISTEN_EXE="K:\\app\\AIVoice\\listen.exe"; python tools\\e2e_hotkey_mode.py

退出码：0 = 全通过；1 = 有失败；2 = 环境不满足（已有 listen.exe 在跑 / 模型缺失）。
"""
import ctypes
import glob
import os
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ctl_client import query as ctl_query  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
# 可用环境变量指定被测 exe（例：装机版 K:\app\AIVoice\listen.exe）
EXE = os.environ.get("LISTEN_EXE") or os.path.join(REPO, "target", "release", "listen.exe")
ROOT = os.path.join(REPO, "target", "repro-mode")
CONFIG_PATH = os.path.join(ROOT, "config.toml")
LOG_DIR = os.path.join(ROOT, "logs")
PIPE = r"\\.\pipe\ListenCtl"
MODEL = r"K:\app\AIVoice\models\paraformer-zh"

CONFIG = """\
[general]
hotkey = "f9"
raw_hotkey = "shift+f9"
hotkey_mode = "hold"
max_session_minutes = 30
segment_seconds = 25
silence_seconds = 0.5
flush_seconds = 60
# 门控阈值拉到 0.9：真实房间底噪永远过不了 → 一个段都不会被送进 ASR，
# 也就不会有注入（E2E 不会往你当前窗口里粘字）。本脚本只关心会话生命周期。
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
enabled = false

[text]
strip_punctuation = true
trailing_space = true
"""

# ---------------------------------------------------------------- SendInput


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


VK_F9 = 0x78


def _send(vk, up):
    inp = _INPUT(type=1, u=_U(ki=_KEYBDINPUT(vk, 0, 2 if up else 0, 0, None)))
    n = ctypes.windll.user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(_INPUT))
    if n != 1:
        raise OSError("SendInput 失败: %d" % ctypes.windll.kernel32.GetLastError())


def keydown(vk=VK_F9):
    _send(vk, False)


def keyup(vk=VK_F9):
    _send(vk, True)


def tap(hold_s=0.25):
    keydown()
    time.sleep(hold_s)
    keyup()


def key_chatter(vk=VK_F9):
    """一次调用里连发 KEYUP+KEYDOWN：模拟键盘抖动 / 别的程序注入的假松开。

    真实用户此刻**仍然按着**键，所以物理状态最后是「按下」。
    """
    arr = (_INPUT * 2)()
    arr[0] = _INPUT(type=1, u=_U(ki=_KEYBDINPUT(vk, 0, 2, 0, None)))
    arr[1] = _INPUT(type=1, u=_U(ki=_KEYBDINPUT(vk, 0, 0, 0, None)))
    n = ctypes.windll.user32.SendInput(2, arr, ctypes.sizeof(_INPUT))
    if n != 2:
        raise OSError("SendInput(chatter) 失败: %d" % ctypes.windll.kernel32.GetLastError())


# ---------------------------------------------------------------- 日志断言

_last_path = {"p": None}
_state = {"n": 0}
_buf = []


def log_path():
    files = glob.glob(os.path.join(LOG_DIR, "app.log.*"))
    if not files:
        return os.path.join(LOG_DIR, "app.log")
    return max(files, key=lambda p: (os.path.getmtime(p), p))


def sync():
    p = log_path()
    if p != _last_path["p"]:
        _last_path["p"] = p
        _state["n"] = 0
    if os.path.exists(p):
        with open(p, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
        if len(lines) < _state["n"]:
            _state["n"] = 0
        _buf.extend(lines[_state["n"]:])
        _state["n"] = len(lines)
    return "\n".join(_buf)


def new_phase():
    _buf.clear()
    sync()


def wait_for(*needles, timeout=8.0):
    """轮询等待任一 needle 出现 → (命中, 累积日志)"""
    end = time.time() + timeout
    while True:
        text = sync()
        if any(n in text for n in needles):
            return True, text
        if time.time() >= end:
            return False, text
        time.sleep(0.2)


def expect_absent(needle, seconds):
    """在 seconds 秒内必须始终看不到 needle"""
    end = time.time() + seconds
    while time.time() < end:
        if needle in sync():
            return False, sync()
        time.sleep(0.2)
    return True, sync()


def check(name, ok, text=""):
    print("  [%s] %-6s %s" % (time.strftime("%H:%M:%S"), "OK" if ok else "FAIL", name))
    if not ok and text:
        for l in text.splitlines()[-14:]:
            print("      | " + l[:180])
    return ok


def audio_secs(text):
    m = re.search(r"会话结束：音频 ([\d.]+)s", text)
    return float(m.group(1)) if m else 0.0


# ---------------------------------------------------------------- 环境


def others_running():
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq listen.exe"],
        capture_output=True,
        text=True,
        errors="replace",
    ).stdout
    return "listen.exe" in out


def ctl(cmd, timeout=3.0, retries=8):
    last = None
    for _ in range(retries):
        r = ctl_query(cmd, timeout)
        if r is not None:
            return r
        last = "busy/timeout"
        time.sleep(0.25)
    return "ERR: %s" % last


def patch_config(key, value):
    """直接改 config.toml（模拟用户拿记事本编辑；不经过设置页/ctl）"""
    with open(CONFIG_PATH, encoding="utf-8") as f:
        text = f.read()
    text = re.sub(r'(?m)^%s\s*=\s*.*$' % re.escape(key), '%s = "%s"' % (key, value), text)
    with open(CONFIG_PATH, "w", encoding="utf-8") as f:
        f.write(text)
    return text


def main():
    if not os.path.isfile(EXE):
        print("!! 找不到 %s（先 cargo build --release）" % EXE)
        return 2
    if not os.path.isdir(MODEL):
        print("!! 找不到本地模型 %s" % MODEL)
        return 2
    if others_running():
        print("!! 已有 listen.exe 在运行：先退出它（托盘 → 退出程序 / ctl_client.py QUIT），")
        print("   否则本脚本启动的实例会撞单实例互斥体并立刻退出。")
        return 2

    # 每次跑都用干净的测试根（旧日志/旧配置不许影响判读）
    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)
    with open(CONFIG_PATH, "w", encoding="utf-8") as f:
        f.write(CONFIG.format(model=MODEL))

    res = []
    print("== 启动实例（%s） ==" % EXE)
    new_phase()
    p = subprocess.Popen([EXE, "--config", CONFIG_PATH, "--no-tray", "--managed"])
    ok, text = wait_for("识别后端就绪", "识别模型已就绪", timeout=90)
    if not ok:
        print("!! 实例没就绪，日志尾部：")
        for l in text.splitlines()[-15:]:
            print("   | " + l[:170])
        p.terminate()
        return 2
    res.append(check("实例就绪（热键钩子已装）", "热键钩子已安装" in text, text))
    time.sleep(1.2)  # 让钩子/采集流先稳定下来

    # ------------------------------------------------ ① toggle：点一下开始
    print("== ① toggle（ctl 切换）：点一下 F9 开始，松手**不许**收尾 ==")
    new_phase()
    r = ctl("SET hotkey_mode toggle")
    ok, text = wait_for("录音方式已切换为「切换", timeout=6)
    res.append(check("SET hotkey_mode toggle 已热生效（%s）" % r, ok, text))
    new_phase()
    tap(0.25)
    ok, text = wait_for("会话开始（切换模式", timeout=8)
    res.append(check("按下 → 会话开始（切换模式）", ok, text))
    alive, text2 = expect_absent("会话结束", 3.5)
    res.append(check("松手后 3.5s 仍在录音（没有「会话结束」）", alive, text2))

    # ------------------------------------------------ ② toggle：再按一下停止
    print("== ② toggle：隔 3s 再点一下 F9 停止 ==")
    new_phase()
    tap(0.25)
    ok1, text = wait_for("切换模式：再次按下", timeout=8)
    res.append(check("第二次按下 → 「切换模式：再次按下」", ok1, text))
    ok2, text2 = wait_for("会话结束：", timeout=8)
    res.append(check("第二次按下 → 会话收尾", ok2, text2 or text))
    secs = audio_secs(text2 or text)
    res.append(
        check(
            "toggle 会话总时长 %.1fs ≥ 3.0s（= 两次按下之间，不是松手就截止）" % secs,
            secs >= 3.0,
            text2 or text,
        )
    )

    # ------------------------------------------------ ③ 手改 config.toml
    print("== ③ 直接改 config.toml（不经设置页）→ 必须自动热生效 ==")
    new_phase()
    r = ctl("SET hotkey_mode hold")  # 先回到 hold，制造一次真实变更
    ok, text = wait_for("录音方式已切换为「按住", timeout=6)
    res.append(check("先切回 hold（ctl，%s）" % r, ok, text))
    time.sleep(0.5)
    new_phase()
    patch_config("hotkey_mode", "toggle")  # 就像拿记事本改 config.toml
    ok, text = wait_for("检测到 config.toml 已被改写", timeout=6)
    res.append(check("worker 自己发现 config.toml 变了", ok, text))
    ok, text = wait_for("录音方式已切换为「切换", timeout=5)
    res.append(check("日志写明「录音方式已切换为切换」", ok, text))
    new_phase()
    tap(0.25)
    ok, text = wait_for("会话开始（切换模式", timeout=8)
    res.append(check("手改文件后按 F9 → 会话是「切换模式」", ok, text))
    new_phase()
    tap(0.25)  # 停掉它
    ok, text = wait_for("会话结束：", timeout=8)
    res.append(check("再按一下正常收尾", ok, text))

    # ------------------------------------------------ ④ hold：按住不平白收尾
    print("== ④ 切回 hold：按住 5s 不松手（不许自己收尾） ==")
    new_phase()
    r = ctl("SET hotkey_mode hold")
    ok, text = wait_for("录音方式已切换为「按住", timeout=6)
    res.append(check("SET hotkey_mode hold 已热生效（%s）" % r, ok, text))
    time.sleep(0.5)
    new_phase()
    keydown()
    ok, text = wait_for("会话开始（按住模式", timeout=8)
    res.append(check("按下 → 会话开始（按住模式）", ok, text))
    alive, text2 = expect_absent("会话结束", 5.0)
    res.append(check("按住 5s 未松手 → 会话仍在（没有「会话结束」）", alive, text2))
    new_phase()
    keyup()
    ok, text = wait_for("会话结束：", timeout=8)
    res.append(check("松手 → 会话收尾", ok, text))

    # ------------------------------------------------ ⑤ hold + 假松开（抖动）
    print("== ⑤ hold：按住时来一对「假 KEYUP+KEYDOWN」（抖动/注入）→ 不许收尾 ==")
    time.sleep(0.6)
    new_phase()
    keydown()
    ok, text = wait_for("会话开始（按住模式", timeout=8)
    res.append(check("按下 → 会话开始（按住模式）", ok, text))
    time.sleep(1.0)
    new_phase()
    key_chatter()
    ok, text = wait_for("忽略 Release", timeout=5)
    res.append(check("识别出「物理键仍按下」的假松开并记 WARN", ok, text))
    alive, text2 = expect_absent("会话结束", 2.0)
    res.append(check("假松开没有结束会话（继续录音）", alive, text2))
    new_phase()
    keyup()
    ok, text = wait_for("会话结束：", timeout=8)
    res.append(check("之后真的松手 → 正常收尾", ok, text))

    # ------------------------------------------------ ⑥ hold + 中途 SUSPEND
    print("== ⑥ hold：录音中收到 SUSPEND（输入法切走）→ 不许截断录音 ==")
    time.sleep(0.6)
    new_phase()
    keydown()
    ok, text = wait_for("会话开始（按住模式", timeout=8)
    res.append(check("按下 → 会话开始（按住模式）", ok, text))
    time.sleep(1.0)
    new_phase()
    print("      SUSPEND -> %s" % ctl("SUSPEND"))
    ok, text = wait_for("收到挂起请求", timeout=5)
    res.append(check("挂起请求被记下（先把这一段录完）", ok, text))
    alive, text2 = expect_absent("会话结束", 2.0)
    res.append(check("SUSPEND 没有截断正在录的这一段", alive, text2))
    new_phase()
    keyup()
    ok, text = wait_for("会话结束：", timeout=8)
    res.append(check("松手 → 正常收尾", ok, text))
    ok, text = wait_for("挂起：钩子与模型常驻", timeout=8)
    res.append(check("收尾之后才真的挂起", ok, text or text2))
    time.sleep(0.3)
    print("      RESUME -> %s" % ctl("RESUME"))
    time.sleep(0.8)

    # ------------------------------------------------ ⑦ hold + 自动重复
    print("== ⑦ hold：按住 10s 并模拟键盘自动重复（~30Hz KEYDOWN） ==")
    new_phase()
    keydown()
    ok, text = wait_for("会话开始（按住模式", timeout=10)
    res.append(check("按下 → 会话开始（按住模式）", ok, text))
    t_end = time.time() + 10.0
    died = False
    while time.time() < t_end:
        keydown()  # 自动重复（真实键盘按住时就是这样）
        time.sleep(0.033)
        if "会话结束" in sync():
            died = True
            break
    text2 = sync()
    res.append(check("自动重复 10s 内会话**没有**被自己收尾", not died, text2))
    new_phase()
    keyup()
    ok, text = wait_for("会话结束：", timeout=8)
    res.append(check("松手 → 会话收尾", ok, text))

    # ------------------------------------------------ 收尾
    print("== 退出 ==")
    print("      QUIT -> %s" % ctl("QUIT"))
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

