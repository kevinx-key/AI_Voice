# e2e_hotkey_suspend.py — 热键/挂起链路 E2E（本机，不需要真的说话）
#
# 验证 2026-10-06 的修复（配合《项目调试过程.md》§16）：
#   1) 单实例护栏：第二个 --managed 必须秒退（不再多开、不再各自装钩子）
#   2) 挂起不摘钩子：SUSPEND 后按 F9 仍要能开录（修「按热键不响应」）
#   3) 预热采集流 + 起头切片：日志应有「采集流已打开」「起头补 Xms/滞后 0ms」
#   4) 没出字必须给反馈：日志「本次未出字（…）」
#
# 注意：日志是非阻塞 writer，且 QUIT 是 process::exit —— 断言一律用
# 「轮询等待」而不是「睡固定秒数后查一次」，否则会偶发假失败。
#
# 用法: python tools\e2e_hotkey_suspend.py
import ctypes
import glob
import os
import subprocess
import sys
import threading
import time

EXE = r"K:\AI_Voice_rs\target\release\listen.exe"
LOG_DIR = r"K:\AI_Voice_rs\target\release\logs"
PIPE = r"\\.\pipe\ListenCtl"

_PUL = ctypes.POINTER(ctypes.c_ulong)


class _KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", ctypes.c_ushort),
        ("wScan", ctypes.c_ushort),
        ("dwFlags", ctypes.c_ulong),
        ("time", ctypes.c_ulong),
        ("dwExtraInfo", _PUL),
    ]


class _U(ctypes.Union):
    _fields_ = [("ki", _KEYBDINPUT), ("pad", ctypes.c_ubyte * 32)]


class _INPUT(ctypes.Structure):
    _fields_ = [("type", ctypes.c_ulong), ("u", _U)]


def _send_key(vk, up):
    """SendInput 合成按键（keybd_event(bScan=0) 实测无效；INPUT 必须是 40 字节）"""
    inp = _INPUT(type=1, u=_U(ki=_KEYBDINPUT(vk, 0, 2 if up else 0, 0, None)))
    n = ctypes.windll.user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(_INPUT))
    if n != 1:
        raise OSError("SendInput 失败: %d" % ctypes.windll.kernel32.GetLastError())


def tap_f9(hold_s=1.2):
    _send_key(0x78, False)
    time.sleep(hold_s)
    _send_key(0x78, True)


def log_path():
    """取最新的 app.log.*（程序按 UTC 日期轮转，脚本不能假设本地日期！
    踩过：本地已经过了零点而 UTC 还在前一天 → 脚本读了个不存在的文件，断言全假失败）"""
    files = glob.glob(os.path.join(LOG_DIR, "app.log.*"))
    if not files:
        return os.path.join(LOG_DIR, "app.log")
    return max(files, key=lambda p: (os.path.getmtime(p), p))


_last_path = {"p": None}


_state = {"n": 0}
_buf = []


def sync():
    """把日志新增行并入 _buf（本阶段累积）；文件换天后自动重置行号"""
    p = log_path()
    if p != _last_path["p"]:
        _last_path["p"] = p
        _state["n"] = 0
    if os.path.exists(p):
        with open(p, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
        if len(lines) < _state["n"]:
            _state["n"] = 0  # 文件被轮转/截断
        _buf.extend(lines[_state["n"]:])
        _state["n"] = len(lines)
    return _buf


def new_phase():
    _buf.clear()
    sync()


def wait_for(*needles, timeout=8.0):
    """轮询等待任一 needle 出现，返回 (命中, 累积日志文本)"""
    end = time.time() + timeout
    while True:
        sync()
        text = "\n".join(_buf)
        if any(n in text for n in needles):
            return True, text
        if time.time() >= end:
            return False, text
        time.sleep(0.3)


def ctl_once(cmd, timeout=3.0):
    box = {}

    def worker():
        try:
            f = open(PIPE, "r+b", buffering=0)
            f.write((cmd + "\n").encode())
            data = b""
            while not data.endswith(b"\n"):
                ch = f.read(1)
                if not ch:
                    break
                data += ch
            f.close()
            box["r"] = data.decode(errors="replace").strip()
        except Exception as e:  # noqa: BLE001
            box["e"] = str(e)

    t = threading.Thread(target=worker, daemon=True)
    t.start()
    t.join(timeout)
    return box.get("r")


def ctl(cmd, timeout=3.0, retries=5):
    """发 ListenCtl 命令。命名管道是单实例：服务端在两次连接之间有窗口，
    客户端会碰到 ERROR_PIPE_BUSY（fork 侧 ListenCtlClient 用 WaitNamedPipe 重试，
    这里同理）——不重试会造成假失败 + 残留进程。"""
    last = None
    for i in range(retries):
        r = ctl_once(cmd, timeout)
        if r is not None:
            return r
        last = "busy/timeout"
        time.sleep(0.3)
    return "ERR: %s" % last


def check(name, ok, text=""):
    print(("  [OK]   " if ok else "  [FAIL] ") + name)
    if not ok and text:
        for l in text.splitlines()[-12:]:
            print("      | " + l[:170])
    return ok


def others_running():
    """是否已有其他 listen.exe 在跑（E2E 会 QUIT 掉占着管道的实例，绝不能误杀用户装机版）"""
    out = subprocess.run(["tasklist", "/FI", "IMAGENAME eq listen.exe"], capture_output=True,
                         text=True, errors="replace").stdout
    return "listen.exe" in out


def main():
    res = []
    if others_running():
        print("!! 已有 listen.exe 在运行（可能是装机版）：先结束它再跑 E2E，"
              "否则本脚本会把它的 ListenCtl 管道实例 QUIT 掉。")
        print("   Stop-Process -Name listen -Force")
        return 2
    sync()
    print("== 启动实例（--managed） ==")
    p1 = subprocess.Popen([EXE, "--managed"])
    new_phase()
    ok, text = wait_for("采集流已打开", timeout=25)
    for name, needle in [
        ("实例启动", "倾听输入法 v0.1.0 启动"),
        ("热键钩子已安装", "热键钩子已安装"),
        ("ListenCtl 管道已就绪", "ListenCtl 管道已就绪"),
        ("识别后端就绪", "识别后端就绪"),
        ("预热采集流已打开", "采集流已打开"),
    ]:
        res.append(check(name, needle in text, text))

    print("== 第二个实例必须秒退 ==")
    new_phase()
    p2 = subprocess.Popen([EXE, "--managed"])
    p2.wait(timeout=30)
    ok, text = wait_for("已有实例在运行", "单实例互斥体不可用", timeout=8)
    res.append(check("第二个实例判定已有实例并退出（exit=%s）" % p2.returncode, ok, text))

    print("== STATUS ==")
    st = ctl("STATUS")
    res.append(check("STATUS 就绪（%s）" % st, st.startswith("OK ready")))

    print("== SUSPEND：钩子必须保留 ==")
    sus = ctl("SUSPEND")
    print("      SUSPEND -> %s" % sus)
    # 单实例命名管道有连接窗口：不靠“一次回复”定序，直接轮询 STATUS 确认已挂起
    st = ""
    for _ in range(20):
        st = ctl("STATUS")
        if st.startswith("OK suspended"):
            break
        time.sleep(0.3)
    print("      STATUS -> %s" % st)
    res.append(check("STATUS 报 suspended（%s）" % st, st.startswith("OK suspended")))
    new_phase()
    ok, text = wait_for("SUSPEND：已挂起（钩子与模型常驻", timeout=8)
    res.append(check("挂起日志（钩子与模型常驻）", ok, text))
    res.append(check("挂起时未摘钩子", "热键钩子线程退出" not in text))

    print("== 挂起状态下按 F9 ==")
    new_phase()
    tap_f9(1.2)
    ok, text = wait_for("本次未出字", timeout=15)
    res.append(check("挂起中收到热键", "挂起中收到热键" in text, text))
    res.append(check("确实开了会话", "会话开始" in text, text))
    res.append(check("会话有起头切片日志", "起头补" in text, text))
    res.append(check("会话统计日志", "会话结束：音频" in text, text))
    res.append(check("静音会话有「未出字」反馈", ok, text))

    print("== 短按 150ms（“点一下再说话”的场景） ==")
    new_phase()
    tap_f9(0.15)
    ok, text = wait_for("短按（按住", timeout=15)
    res.append(check("短按给出 1.5s 继续听窗口", ok, text))
    ok2, text2 = wait_for("会话结束：音频", timeout=10)
    res.append(check("短按也开会话并收尾", ok2, text2))

    print("== RESUME + QUIT ==")
    print("      RESUME -> %s" % ctl("RESUME"))
    time.sleep(0.8)
    print("      QUIT   -> %s" % ctl("QUIT"))
    time.sleep(1.5)
    if p1.poll() is None:
        p1.terminate()
    print("\n== 结果: %d/%d 通过 ==" % (sum(1 for r in res if r), len(res)))
    return 0 if all(res) else 1


if __name__ == "__main__":
    sys.exit(main())
