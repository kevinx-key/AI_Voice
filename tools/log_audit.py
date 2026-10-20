#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""log_audit.py —— 倾听输入法运行日志对账（装机实测用）

为什么需要它：`热键事件转发 Press` 与 worker 受理的记录必须**逐进程**配对，
否则跨进程重启后 seq 会重复，集合比较会给出**假阳性/假阴性**。
（2026-10-08 实测：不按进程分段时会误报“按下被吞”；按进程分段后发现是
 toggle 模式的“停止”分支当时静默无日志 —— 该日志缺失已修。）

用法:
    python tools\\log_audit.py                      # 审计最新的装机日志
    python tools\\log_audit.py --log <路径>
    python tools\\log_audit.py --since 2026-10-08T11:00
"""
import argparse
import collections
import glob
import os
import re
import sys

LOG_DIRS = [r"K:\app\AIVoice\logs", os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), "target", "release", "logs")]

# worker 受理一次“按下”的三条合法落点（缺一不可，否则对账会假阳性）
ACCEPT_PATS = [
    r"热键按下 seq=(\d+)",
    r"会话中再次按下 seq=(\d+)",
    r"切换模式：再次按下 seq=(\d+)",
]


def newest_log(explicit, since):
    if explicit:
        return explicit
    cands = []
    for d in LOG_DIRS:
        cands += glob.glob(os.path.join(d, "app.log.*"))
    if since:  # 只要包含 since 之后日志的文件就够，实际过滤按行做
        pass
    if not cands:
        sys.exit("没找到 app.log.*（用 --log 指定）")
    cands.sort(key=lambda p: os.path.getmtime(p), reverse=True)
    return cands[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", default=None)
    ap.add_argument("--since", default=None, help="只看这个时间之后，如 2026-10-08T11:00")
    a = ap.parse_args()
    p = newest_log(a.log, a.since)
    L = [l for l in open(p, encoding="utf-8", errors="replace")
         if re.match(r"^2026-\d\d-\d\dT", l)]
    if a.since:
        L = [l for l in L if l[:16] >= a.since]
    if not L:
        sys.exit("日志为空（或被 --since 过滤空了）")

    print("=" * 100)
    print("日志 %s\n时间 %s ~ %s   行数 %d" % (p, L[0][:23], L[-1][:23], len(L)))
    print("=" * 100)

    # ---------- 按进程分段对账 ----------
    # 「已有实例在运行，本次直接退出」的**空进程也会打启动行**（2026-10-08 实测：
    # 它一开一个新 run，后面真进程的事件就被挂到它名下，看起来像“另一个进程才有的按下”）。
    # 所以只有“启动行之后不是立刻被单实例挡下”的才是真进程。
    blocked_next = {i - 1 for i, l in enumerate(L) if "已有实例在运行" in l}
    runs, cur = [], None
    for i, l in enumerate(L):
        if "v0.1.0 启动" in l and i not in blocked_next:
            cur = {"t": l[11:19], "lines": []}
            runs.append(cur)
        if cur is not None:
            cur["lines"].append(l)

    print("\n【热键对账（逐进程）】")
    print("  %-22s %-6s %-6s %-16s %s" % ("进程启动时刻", "转发", "受理", "不一致", "判定"))
    bad = 0
    for i, r in enumerate(runs, 1):
        fwd, acc = [], []
        for l in r["lines"]:
            if m := re.search(r"热键事件转发 Press seq=(\d+)", l):
                fwd.append(int(m.group(1)))
            for pat in ACCEPT_PATS:
                if m := re.search(pat, l):
                    acc.append(int(m.group(1)))
                    break
        if not fwd and not acc:
            continue  # 被单实例挡下的空进程
        miss = sorted(set(fwd) - set(acc))
        extra = sorted(set(acc) - set(fwd))
        dup = sorted(s for s in set(fwd) if fwd.count(s) > 1)
        ok = not (miss or extra or dup)
        if not ok:
            bad += 1
        print("  #%-2d %-19s %-6d %-6d %-16s %s"
              % (i, r["t"], len(fwd), len(acc),
                 ("丢%s 多%s 重%s" % (miss or "-", extra or "-", dup or "-")) if not ok else "-",
                 "OK" if ok else "!! 有按下没落点（先看该进程是不是旧二进制：老版本 toggle 停止支路无日志）"))
    print("  → %s" % ("全部进程零吞键 ✅" if bad == 0 else "%d 个进程存在不一致 ⚠️" % bad))

    # ---------- 单实例 ----------
    starts = [l for l in L if "v0.1.0 启动" in l]
    blocked = [l for l in L if "已有实例在运行" in l]
    print("\n【单实例护栏】启动尝试 %d 次，挡下 %d 次" % (len(starts), len(blocked)))

    # ---------- 钩子活性 ----------
    print("\n【钩子活性 / 丢 KEYUP】")
    heal = 0
    zero_windows = 0
    for l in L:
        if m := re.search(r"热键钩子心跳：近 60s 回调 (\d+) 次（命中主键 (\d+) 次，"
                          r"回调内自愈 KEYUP 丢失 (\d+) 次）", l):
            c, v, h = map(int, m.groups())
            heal = max(heal, h)
            if c == 0:
                zero_windows += 1
    selfheal5 = [l for l in L if "KEYUP 丢失，已自愈" in l]
    blockedmod = [l for l in L if "被修饰键挡下" in l]
    print("  回调内自愈峰值 %d 次；5s 兜底自愈 %d 次；被修饰键挡下 %d 次；回调为 0 的 60s 窗口 %d 个"
          % (heal, len(selfheal5), len(blockedmod), zero_windows))
    for l in selfheal5[:5]:
        print("      ", l[11:23], l.strip()[-70:])

    # ---------- 延迟 / GUI ----------
    lagf = [int(m.group(1)) for l in L if (m := re.search(r"钩子→转发滞后 (\d+)ms", l))]
    lagw = [int(m.group(1)) for l in L if (m := re.search(r"钩子→worker 滞后 (\d+)ms", l))]
    gui = [l for l in L if "GUI 帧间隔" in l]
    print("\n【延迟 / GUI】转发滞后 max=%sms(n=%d)；worker 滞后 max=%sms(n=%d)"
          % (max(lagf) if lagf else "-", len(lagf), max(lagw) if lagw else "-", len(lagw)))
    print("  GUI 帧间隔 >300ms: %d 次" % len(gui))
    for l in gui[:6]:
        print("      ", l[11:23], l.strip()[-60:])

    # ---------- 会话 / 出字 ----------
    sess = [l for l in L if "会话开始（" in l]
    mode = collections.Counter("toggle" if "切换模式" in l else "hold" for l in sess)
    pre = [int(m.group(1)) for l in L if (m := re.search(r"起头补 (\d+)ms", l))]
    chars = segs = halu = 0
    ms = []
    for l in L:
        if m := re.search(r"识别\((\d+)ms, ([\d.]+)s, 切分 \w+\): (.*)", l):
            ms.append(int(m.group(1)))
            segs += 1
            chars += len(m.group(3))
        if "丢弃重复堆叠幻觉" in l:
            halu += 1
    print("\n【会话 / 出字】会话 %d 次 模式=%s" % (len(sess), dict(mode)))
    print("  起头补 pre-roll: %d/%d 次为 0ms %s"
          % (pre.count(0), len(pre), "（0ms = 无 pre-roll，起头会丢字）" if pre.count(0) else "✅"))
    print("  识别段 %d / 总字 %d；单段 ms 中位 %s max %s；丢弃重复堆叠幻觉 %d 次"
          % (segs, chars, sorted(ms)[len(ms) // 2] if ms else "-", max(ms) if ms else "-", halu))

    # ---------- 未出字原因 ----------
    cnt = collections.Counter()
    for l in L:
        if m := re.search(r"本次未出字（(.+?)）", l):
            cnt[m.group(1)] += 1
        if "疑似静音，跳过" in l:
            cnt["段过轻被闸(疑似静音)"] += 1
    if cnt:
        print("\n【未出字原因】")
        for k, v in cnt.most_common():
            print("   %3dx %s" % (v, k))

    # ---------- 错误 ----------
    print("\n【错误/警告汇总】")
    for lvl in ("ERROR", "WARN"):
        c = collections.Counter()
        for l in L:
            if " %s " % lvl in l:
                c[re.sub(r"\d+", "N", re.sub(r"^.*?%s \S+: " % lvl, "", l).strip())[:72]] += 1
        print("  %s %d 条" % (lvl, sum(c.values())))
        for k, v in c.most_common(6):
            print("     %3dx %s" % (v, k))
    print("=" * 100)


if __name__ == "__main__":
    main()
