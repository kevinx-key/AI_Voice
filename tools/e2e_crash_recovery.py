# -*- coding: utf-8 -*-
# ⚠️ 本脚本会**模拟鼠标/键盘**（SendInput / mouse_event / SendKeys）或置顶窗口。
#    按本机全局规则（~/.dsh/AGENTS.md 第一节），**agent 不得运行它**，只能在用户明确同意时
#    由用户自己手动执行。agent 侧的回归只做：cargo test + 文件/日志核验。
r"""e2e_crash_recovery.py —— 崩溃自愈链路 E2E（不用真说话，不用人守）。

为什么有它（2026-10-20 用户报的第二件事）：
    「设置页面自动关闭之后整个语音引擎都无法使用，只有重新在设置中打开语音、
     打开设置它才自动再把整个引擎系统拉起来」

现场日志的特征是**一行收尾都没有**（没有 QUIT / 没有工作线程退出 / 无 WER 记录），
最后一行是钩子心跳然后直接断掉 —— 典型的「进程被无声带走」（主线程 panic 的可能性最大，
而 GUI 与录音引擎同进程，GUI 一死引擎就死）。

修法（详见主程序 `install_crash_guard`）：
    ① panic 钩子**同步**写 `logs\last_panic.txt`（线程名 + 位置 + backtrace）
    ② 主线程 / worker 线程崩溃 → 立刻拉起新实例（`--retry-lock` 等旧进程释放单实例锁）
    ③ 只自启一轮（子进程带 `LISTEN_RESPAWN=1`），防崩溃死循环
    ④ 新进程启动时把「上次异常退出」提示给用户（悬浮窗 + 运行日志）

本脚本用 `LISTEN_PANIC_TEST=<秒>`（仅测试用环境变量）让 worker 线程在 N 秒后故意 panic，
然后断言上面 4 条都真的发生。

用法：
    python tools\ctl_client.py QUIT          # 先退出正在运行的倾听输入法
    python tools\e2e_crash_recovery.py

退出码：0 = 全通过；1 = 有失败；2 = 环境不满足。
"""
import glob
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ctl_client import query as ctl_query  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
EXE = os.environ.get("LISTEN_EXE") or os.path.join(REPO, "target", "release", "listen.exe")
ROOT = os.path.join(REPO, "target", "repro-crash")
CONFIG_PATH = os.path.join(ROOT, "config.toml")
LOG_DIR = os.path.join(ROOT, "logs")
PANIC_FILE = os.path.join(LOG_DIR, "last_panic.txt")
PANIC_PREV = os.path.join(LOG_DIR, "last_panic.prev.txt")
MODEL = r"K:\app\AIVoice\models\paraformer-zh"

CONFIG = """\
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
enabled = false
"""


def listen_pids():
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq listen.exe", "/FO", "CSV", "/NH"],
        capture_output=True,
        text=True,
        errors="replace",
    ).stdout
    pids = []
    for line in out.splitlines():
        parts = [p.strip('"') for p in line.split('","')]
        if len(parts) >= 2 and parts[0].lower().startswith("listen.exe"):
            try:
                pids.append(int(parts[1]))
            except ValueError:
                pass
    return pids


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


def wait_until(pred, timeout, step=0.3):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(step)
    return pred()


def check(name, ok, detail=""):
    print("  [%s] %-6s %s" % (time.strftime("%H:%M:%S"), "OK" if ok else "FAIL", name))
    if not ok and detail:
        for l in str(detail).splitlines()[-12:]:
            print("      | " + l[:180])
    return ok


def main():
    res = []
    if not os.path.isfile(EXE):
        print("!! 找不到 %s（先 cargo build --release）" % EXE)
        return 2
    if not os.path.isdir(MODEL):
        print("!! 找不到本地模型 %s" % MODEL)
        return 2
    others = listen_pids()
    if others:
        print("!! 已有 listen.exe 在运行（PID %s）：先退出它再跑本脚本" % others)
        return 2

    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)
    with open(CONFIG_PATH, "w", encoding="utf-8") as f:
        f.write(CONFIG.format(model=MODEL))

    # ------------------------------------------------ ① 故意崩一次
    print("== ① 起一个实例，4 秒后在 worker 线程故意 panic（LISTEN_PANIC_TEST=4） ==")
    env = dict(os.environ)
    env["LISTEN_PANIC_TEST"] = "4"
    p1 = subprocess.Popen(
        [EXE, "--config", CONFIG_PATH, "--no-tray", "--managed"], env=env
    )
    res.append(check("实例已启动（pid=%s）" % p1.pid, True))
    started = time.time()
    died = wait_until(lambda: p1.poll() is not None, timeout=40)
    res.append(check("worker 线程 panic 后进程退出（%.1fs）" % (time.time() - started), died))

    # ------------------------------------------------ ② 崩溃现场
    print("== ② 崩溃现场必须落到 logs\\last_panic.txt ==")
    ok = wait_until(lambda: os.path.isfile(PANIC_FILE) or os.path.isfile(PANIC_PREV), 5)
    path = PANIC_FILE if os.path.isfile(PANIC_FILE) else PANIC_PREV
    text = ""
    if ok:
        with open(path, encoding="utf-8", errors="replace") as f:
            text = f.read()
    res.append(check("写了崩溃现场文件（%s）" % os.path.basename(path), ok and "PANIC" in text, text))
    res.append(check("现场里有线程名 / 位置 / 模拟崩溃信息",
                     ("worker" in text) and ("位置" in text) and ("模拟崩溃" in text),
                     text[:600]))

    # ------------------------------------------------ ③ 自动拉起新实例
    print("== ③ 必须自动拉起一个新实例（旧进程的 PID 之外的 listen.exe） ==")
    new_pid = {"pid": None}

    def has_new():
        for pid in listen_pids():
            if pid != p1.pid:
                new_pid["pid"] = pid
                return True
        return False

    ok = wait_until(has_new, timeout=20)
    res.append(check("自动重启成功（新 pid=%s）" % new_pid["pid"], ok, log_text()[-800:]))

    # ------------------------------------------------ ④ 新进程给出提示
    print("== ④ 新进程要告诉用户「上次异常退出，已自动重启」 ==")
    ok = wait_until(lambda: "上次运行异常退出" in log_text(), timeout=20)
    res.append(check("新进程日志里有「上次运行异常退出」提示", ok, log_text()[-1200:]))
    ok = wait_until(lambda: os.path.isfile(PANIC_PREV) and not os.path.isfile(PANIC_FILE), 5)
    res.append(check("现场已改名留档（last_panic.prev.txt），不会每次启动都提示", ok))

    # ------------------------------------------------ ⑤ 再崩一次不许无限重启
    print("== ⑤ 带 LISTEN_RESPAWN=1 再崩一次 → 不许再自启（防崩溃死循环） ==")
    # 先把当前实例退出，再起一个「已经是重启产物」的进程
    ctl_query("QUIT", 2.0)
    wait_until(lambda: not listen_pids(), timeout=10)
    time.sleep(0.6)
    env2 = dict(os.environ)
    env2["LISTEN_PANIC_TEST"] = "3"
    env2["LISTEN_RESPAWN"] = "1"
    p2 = subprocess.Popen([EXE, "--config", CONFIG_PATH, "--no-tray", "--managed"], env=env2)
    wait_until(lambda: p2.poll() is not None, timeout=40)
    time.sleep(6)  # 给「不该发生的自启」留足时间
    leftover = [pid for pid in listen_pids() if pid != p2.pid]
    res.append(check("第二次崩溃没有再拉起新进程（剩 %s）" % leftover, not leftover, log_text()[-600:]))
    for pid in leftover:
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)

    print("\n== 结果: %d/%d 通过 ==" % (sum(1 for r in res if r), len(res)))
    return 0 if all(res) else 1


if __name__ == "__main__":
    sys.exit(main())
