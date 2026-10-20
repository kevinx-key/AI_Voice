#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""record_gate.py —— 噪声回归集采样引导（tests\\gate）

为什么需要它：四个类别各要 3~5 条，手敲 15 次命令行既烦又容易记错编号；
更要紧的是「两个 F9 钩子并存」这个坑（装机版没退干净时，按一下 F9 会同时
触发它往当前窗口注字）—— 本脚本在**每一条开录前**检查一次，发现外部
listen.exe 就报出来，避免采到被污染的样本。

用法：
    python tools\\record_gate.py                     # 四类各录到 3 条（已有的不重录）
    python tools\\record_gate.py --per-class 5       # 每类录到 5 条
    python tools\\record_gate.py --cls keyboard      # 只录某一类
    python tools\\record_gate.py --list              # 只体检现有样本，不录
    python tools\\record_gate.py --force             # 已知有外部实例也硬录（不推荐）

录完接：
    python tools\\gate_bench.py run --tag before-vad
"""
import argparse
import array
import math
import os
import re
import subprocess
import sys
import wave

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(REPO, "target", "release", "listen.exe")
GATE = os.path.join(REPO, "tests", "gate")

# cls: (录制提示, 时长下限, 时长上限)
CLASSES = {
    "noise_only": ("别说任何话、别碰键鼠：按住 F9 保持 3~5 秒后松手", 2.5, 6.0),
    "keyboard": ("按住 F9 不放，同时连续敲键盘/点鼠标 3~5 秒（不说法），然后松手", 2.5, 6.0),
    "noise_soft_speech": ("按住 F9，用**比平时小一档**的音量说满一整句（2~4 秒），说完松手", 0.8, 8.0),
    "normal_speech": ("按住 F9，用**正常音量**说满一整句（3~5 秒），说完松手", 0.8, 8.0),
}
ORDER = ["noise_only", "keyboard", "noise_soft_speech", "normal_speech"]


def stat_wav(path):
    """→ (时长s, RMS, 峰值)"""
    with wave.open(path, "rb") as w:
        n, ch, sw, sr = w.getnframes(), w.getnchannels(), w.getsampwidth(), w.getframerate()
        raw = w.readframes(n)
    if sw != 2 or not raw:
        return None
    a = array.array("h")
    a.frombytes(raw)
    if ch > 1:
        a = a[::ch]
    peak = max(abs(x) for x in a) / 32768.0
    rms = math.sqrt(sum(float(x) * x for x in a) / len(a)) / 32768.0
    return n / sr, rms, peak


def files_of(cls):
    d = os.path.join(GATE, cls)
    if not os.path.isdir(d):
        return []
    return sorted(f for f in os.listdir(d) if f.lower().endswith(".wav"))


def next_names(cls, count):
    """按现有编号续号：01.wav / 02.wav …（跳过目录里已有的数字前缀）"""
    used = set()
    for f in files_of(cls):
        m = re.match(r"^(\d+)", f)
        if m:
            used.add(int(m.group(1)))
    out, i = [], 1
    while len(out) < count:
        if i not in used:
            out.append(os.path.join(GATE, cls, "%02d.wav" % i))
        i += 1
    return out


def foreign_listen():
    """列出正在运行的 listen.exe（本脚本自己刚拉起的那个不算——调用点已排除）"""
    ps = ("Get-Process listen -ErrorAction SilentlyContinue | "
          "ForEach-Object { \"$($_.Id)|$($_.Path)\" }")
    try:
        r = subprocess.run(["powershell", "-NoProfile", "-Command", ps],
                           capture_output=True, text=True, errors="replace", timeout=30)
    except Exception:
        return []
    return [l.strip() for l in (r.stdout or "").splitlines() if l.strip()]


def noise_ref():
    """现有 noise_only 样本的中位 RMS / 峰值，作为"底噪量级"参照"""
    rs, ps = [], []
    for f in files_of("noise_only"):
        s = stat_wav(os.path.join(GATE, "noise_only", f))
        if s:
            rs.append(s[1])
            ps.append(s[2])
    if not rs:
        return None, None
    rs.sort()
    ps.sort()
    return rs[len(rs) // 2], ps[len(ps) // 2]


def verdict(cls, secs, rms, peak, nrms, npeak):
    """采样体检：只做"这条样本有没有代表性"的粗判，绝对阈值本就该由数据集自己定"""
    flags = []
    _, lo, hi = CLASSES[cls]
    if secs < lo or secs > hi:
        flags.append("时长 %.1fs 不在 %.1f~%.1fs" % (secs, lo, hi))
    if nrms:
        ratio = rms / nrms
        if cls == "noise_only" and ratio > 3:
            flags.append("比现有底噪吵 %.1f 倍" % ratio)
        elif cls == "keyboard" and npeak and peak / npeak < 5:
            flags.append("没录到敲击（峰值仅 %.1f 倍底噪）" % (peak / npeak))
        elif cls == "noise_soft_speech" and ratio < 2:
            flags.append("基本等于底噪（%.1f 倍），可能太小" % ratio)
        elif cls == "normal_speech" and ratio < 8:
            flags.append("离底噪太近（%.1f 倍），不像正常音量" % ratio)
    return flags


def list_only():
    nrms, npeak = noise_ref()
    print("底噪参照（现有 noise_only 中位）: RMS %.5f / 峰值 %.4f\n" % (nrms or 0, npeak or 0))
    print("%-20s %-28s %7s %9s %9s  %s" % ("类别", "文件", "时长s", "RMS", "峰值", "体检"))
    print("-" * 92)
    for cls in ORDER:
        for f in files_of(cls):
            s = stat_wav(os.path.join(GATE, cls, f))
            if not s:
                print("%-20s %-28s  （不是 16bit wav？）" % (cls, f))
                continue
            v = verdict(cls, s[0], s[1], s[2], nrms, npeak)
            print("%-20s %-28s %7.2f %9.5f %9.4f  %s"
                  % (cls, f, s[0], s[1], s[2], "；".join(v) if v else "OK"))
    print()
    for cls in ORDER:
        n = len(files_of(cls))
        print("  %-20s %d/3 条 %s" % (cls, n, "✅" if n >= 3 else "⬅ 还差 %d 条" % (3 - n)))


def main():
    ap = argparse.ArgumentParser(description="噪声回归集采样引导")
    ap.add_argument("--cls", action="append", choices=ORDER, help="只录指定类别（可多写）")
    ap.add_argument("--per-class", type=int, default=3, help="每类目标条数（默认 3）")
    ap.add_argument("--exe", default=EXE)
    ap.add_argument("--force", action="store_true", help="有外部 listen.exe 也硬录")
    ap.add_argument("--redo", action="store_true",
                    help="重录已存在的 01.wav/02.wav…（原地覆盖；带描述名的文件如 00_本机底噪2.5s.wav 不动）")
    ap.add_argument("--list", action="store_true", help="只体检现有样本")
    ap.add_argument("--plan", action="store_true", help="只打印将要录的文件清单，不录")
    a = ap.parse_args()

    if a.list:
        return list_only()
    if not os.path.isfile(a.exe):
        sys.exit("找不到 %s —— 先 cargo build --release" % a.exe)

    todo = []
    for cls in (a.cls or ORDER):
        files = files_of(cls)
        if a.redo:
            # 原地重录纯编号文件（旧样本可能是被 3s 环形缓冲截断的废样）
            for f in files:
                if re.fullmatch(r"\d+\.wav", f, re.I):
                    todo.append((cls, os.path.join(GATE, cls, f)))
        lack = max(0, a.per_class - len(files))
        for p in next_names(cls, lack):
            todo.append((cls, p))
    if not todo:
        print("四类都够 %d 条了，不用再录。要重录请先删掉旧文件（或改 --per-class）。" % a.per_class)
        return list_only()

    if a.plan:
        for i, (cls, p) in enumerate(todo, 1):
            print("%2d) %-20s %s" % (i, cls, os.path.basename(p)))
        return

    nrms, npeak = noise_ref()
    print("=" * 92)
    print("待录 %d 条：%s" % (len(todo), "  ".join("%s/%s" % (c, os.path.basename(p)) for c, p in todo)))
    print("底噪参照: RMS %.5f / 峰值 %.4f" % (nrms or 0, npeak or 0))
    print("=" * 92)
    print("提示：开录后先在控制台看到「按住「f9」开始录」再按住 F9。每条录完立刻体检。\n")

    bad = []
    for i, (cls, out) in enumerate(todo, 1):
        others = foreign_listen()
        if others and not a.force:
            what = "，".join(others)
            print("\n⚠️ 检测到别的 listen.exe 在跑：%s" % what)
            print("   （同一时刻会有两个 F9 钩子，按一下它会往当前窗口注字；建议托盘退出它再继续）")
            if input("   仍要继续？（y/N）").strip().lower() != "y":
                print("已中止。退出外部实例后重跑本脚本即可（已完成的不重录）。")
                break
        print("[%d/%d] %s/%s" % (i, len(todo), cls, os.path.basename(out)))
        print("   %s" % CLASSES[cls][0])
        os.makedirs(os.path.dirname(out), exist_ok=True)
        p = subprocess.Popen([a.exe, "--record", out], stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, text=True, encoding="utf-8",
                             errors="replace", bufsize=1)
        wrote = False
        for line in p.stdout:
            line = line.rstrip()
            if line:
                print("     · %s" % line)
            if "已写出" in line:
                wrote = True
        p.wait()
        if p.returncode != 0 or not wrote or not os.path.isfile(out):
            print("   ✗ 这条没录成（exit=%s）——重跑本脚本会自动续到同一编号" % p.returncode)
            bad.append("%s/%s" % (cls, os.path.basename(out)))
            continue
        s = stat_wav(out)
        v = verdict(cls, s[0], s[1], s[2], nrms, npeak)
        print("   → 时长 %.2fs / RMS %.5f / 峰值 %.4f  %s"
              % (s[0], s[1], s[2], "OK" if not v else "⚠️ " + "；".join(v)))
        if v:
            bad.append("%s/%s（%s）" % (cls, os.path.basename(out), "；".join(v)))

    print("\n" + "=" * 92)
    list_only()
    if bad:
        print("需要重录的：")
        for b in bad:
            print("  - %s" % b)
        print("重录办法：删掉该文件再跑本脚本（会补到那个编号）。")
    print("\n下一步：python tools\\gate_bench.py run --tag before-vad")
    print("记得把装机版拉回来（或切一下输入法让 fork 自己拉）。")


if __name__ == "__main__":
    main()
