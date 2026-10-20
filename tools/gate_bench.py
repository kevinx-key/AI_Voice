#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""gate_bench.py —— 门控/分段回归跑分与基线对比（倾听输入法）

为什么需要它：改门控（能量阈值 → VAD）时，"感觉好点了"是不可靠的验收标准。
本脚本把"改前 vs 改后"变成可比数字，并按事先定死的硬指标判 PASS/FAIL。

用法：
    # 1) 跑基线（建议指定装机配置，数字才对应线上行为）
    python tools\\gate_bench.py run --tag before-vad
    python tools\\gate_bench.py run --config K:\\app\\AIVoice\\config.toml --tag before-vad

    # 2) 改完再用同一目录、同一配置跑一次
    python tools\\gate_bench.py run --tag after-vad

    # 3) 对比判定（硬指标见 CRITERIA）
    python tools\\gate_bench.py compare tests\\gate\\gate-after-vad.json tests\\gate\\gate-before-vad.json

目录约定：tests\\gate\\<类别>\\*.wav，四个类别（缺哪个就跳过哪个）：
    noise_only          纯底噪（不说法）           → 出字必须 0
    keyboard            键盘敲击/鼠标点击          → 出字必须 0
    noise_soft_speech   底噪里轻声说话             → 出字不能掉
    normal_speech       正常说话                   → 出字不能掉、段数不能暴涨
"""
import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(REPO, "target", "release", "listen.exe")
GATE_DIR = os.path.join(REPO, "tests", "gate")
INSTALLED_CFG = r"K:\app\AIVoice\config.toml"

# 硬指标（改完必须满足，否则判 FAIL）
#   (类别, 指标, 比较方式, 阈值, 说明)
CRITERIA = [
    ("noise_only", "chars", "eq", 0, "纯底噪不允许出任何字（治“底噪成字”）"),
    ("keyboard", "chars", "eq", 0, "键盘/鼠标瞬变不允许出任何字（幻觉燃料）"),
    ("noise_soft_speech", "chars", "min_ratio", 0.90, "轻声说话不能被吃掉（>= 基线的 90%）"),
    ("noise_soft_speech", "chars", "gt", 0, "该类必须真的能出字，否则说明门控全关"),
    ("normal_speech", "chars", "min_ratio", 0.95, "正常说话召回不能掉（>= 基线的 95%）"),
    ("normal_speech", "segs", "max_ratio", 1.50, "段数不能暴涨（暴涨 = 碎片化 = 幻觉风险）"),
]


def find_config(explicit):
    if explicit:
        return explicit
    if os.path.isfile(INSTALLED_CFG):
        return INSTALLED_CFG
    return os.path.join(REPO, "config.toml")


def run_gate(cfg_path, gate_dir):
    """调用 listen.exe --gate，解析 TSV。返回 (header, cols, rows, 原始输出)

    列名从 TSV 头行读（`cls\tfile\tcuts\t…`），不再写死下标 ——
    2026-10-08 加「送 ASR 段数/asr_segs」时就是被写死的下标卡住了。
    """
    if not os.path.isfile(EXE):
        sys.exit("找不到 %s —— 先 cargo build --release" % EXE)
    cmd = [EXE, "--gate", gate_dir, "--config", cfg_path]
    p = subprocess.run(cmd, capture_output=True, text=True, errors="replace")
    out = (p.stdout or "") + (p.stderr or "")
    header, cols, rows = None, [], []
    for line in out.splitlines():
        if line.startswith("# 门控基线"):
            header = line
        elif line.startswith("cls\t"):
            cols = line.split("\t")
        elif line.startswith("SUM\t"):
            rows.append(("SUM", line.split("\t")))
        elif line and "\t" in line:
            rows.append(("ROW", line.split("\t")))
    if not rows:
        sys.exit("没解析到任何结果，原始输出：\n" + out[-3000:])
    if not cols:
        sys.exit("TSV 头行缺失（--gate 输出格式变了？）\n" + out[:1500])
    return header, cols, rows, out


def row_dict(cols, f):
    """按列名把一行 TSV 变成 dict（数值列自动转 int/float）"""
    d = {}
    for k, v in zip(cols, f):
        if k in ("cls", "file", "text"):
            d[k] = v
        else:
            try:
                d[k] = float(v) if "." in v else int(v)
            except ValueError:
                d[k] = v
    return d


def summarize(rows):
    """→ {cls: {files, secs, segs, skipped, chars, asr_ms, chars_per_seg}}"""
    agg = {}
    for kind, f in rows:
        if kind != "SUM":
            continue
        cls = f[1]
        d = {}
        for tok in f[2:]:
            if "=" not in tok:
                continue
            k, v = tok.split("=", 1)
            try:
                d[k] = float(v) if "." in v else int(v)
            except ValueError:
                d[k] = v
        agg[cls] = d
    return agg


def cmd_run(args):
    cfg = find_config(args.config)
    header, cols, rows, raw = run_gate(cfg, args.gate_dir)
    agg = summarize(rows)
    # 明细也留档，便于事后查"哪一条幻觉了"
    detail = [row_dict(cols, f) for kind, f in rows if kind == "ROW"]

    print(header)
    print()
    # 「送 ASR」才是幻觉暴露面：2026-10-08 实测键盘噪声段**全穿过了能量门**，
    # 只是 Paraformer 恰好返回空串（不算出字、也不算被闸）—— 旧表里这个数看不见。
    print("%-20s %6s %8s %6s %6s %7s %6s %7s %7s"
          % ("类别", "文件", "音频s", "切段", "被闸", "送ASR", "出字", "字/段", "尾段丢弃"))
    print("-" * 84)
    for cls in sorted(agg):
        a = agg[cls]
        print(
            "%-20s %6s %8.1f %6s %6s %7s %6s %7.1f %7s"
            % (cls, a.get("files", 0), a.get("secs", 0), a.get("cuts", 0),
               a.get("skipped", 0), a.get("asr_segs", 0), a.get("chars", 0),
               a.get("chars_per_seg", 0), a.get("tail_dropped", 0))
        )

    tag = args.tag or datetime.now().strftime("%Y%m%d-%H%M")
    os.makedirs(GATE_DIR, exist_ok=True)
    dest = os.path.join(GATE_DIR, "gate-%s.json" % tag)
    with open(dest, "w", encoding="utf-8") as fp:
        json.dump(
            {"tag": tag, "config": cfg, "header": header, "summary": agg, "detail": detail},
            fp, ensure_ascii=False, indent=2,
        )
    print("\n基线已存盘: %s" % dest)
    # 最该盯的两件事：① 纯噪声出字（幻觉真的发生了）② 纯噪声段被送进 ASR（暴露面，C1 要压的就是它）
    for d in detail:
        if d["cls"] not in ("noise_only", "keyboard"):
            continue
        if d["chars"] > 0:
            print("  !! %s/%s 纯噪声却出字 %d 字: %s" % (d["cls"], d["file"], d["chars"], d["text"][:60]))
        if (d.get("asr_segs") or 0) > 0:
            print("  -- %s/%s 有 %s 段（共切 %s 段）被送进 ASR（%.0fms）← 幻觉暴露面"
                  % (d["cls"], d["file"], d["asr_segs"], d.get("cuts"), d.get("asr_ms", 0)))
    return 0


def cmd_compare(args):
    with open(args.after, encoding="utf-8") as fp:
        after = json.load(fp)
    with open(args.before, encoding="utf-8") as fp:
        before = json.load(fp)
    print("改后: %s" % args.after)
    print("改前: %s\n" % args.before)

    ok = True
    print("%-20s %-9s %12s %12s %8s  %s" % ("类别", "指标", "改前", "改后", "变化", "判定"))
    print("-" * 88)
    for cls, metric, mode, thr, why in CRITERIA:
        b = before["summary"].get(cls, {}).get(metric)
        a = after["summary"].get(cls, {}).get(metric)
        if b is None or a is None:
            print("%-20s %-9s %12s %12s %8s  SKIP（缺数据）" % (cls, metric, b, a, "-"))
            continue
        if mode == "eq":
            passed = a == thr
        elif mode == "gt":
            passed = a > thr
        elif mode == "min_ratio":
            passed = a >= b * thr
        elif mode == "max_ratio":
            passed = a <= b * thr
        else:
            passed = False
        ratio = ("%.2fx" % (a / b)) if b else "-"
        tag = "PASS" if passed else "FAIL"
        ok = ok and passed
        print("%-20s %-9s %12s %12s %8s  %s  %s" % (cls, metric, b, a, ratio, tag, why))

    print("-" * 88)
    print("总判定: %s" % ("PASS —— 可以装机" if ok else "FAIL —— 不许装机，先查回归"))
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description="门控回归跑分/对比")
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="跑一次跑分并存基线")
    r.add_argument("--dir", dest="gate_dir", default=GATE_DIR)
    r.add_argument("--config", default=None, help="默认优先用装机配置 %s" % INSTALLED_CFG)
    r.add_argument("--tag", default=None, help="存盘标签，如 before-vad / after-vad")
    r.set_defaults(func=cmd_run)

    c = sub.add_parser("compare", help="按硬指标对比两次结果")
    c.add_argument("before")
    c.add_argument("after")
    c.set_defaults(func=cmd_compare)

    args = ap.parse_args()
    sys.exit(args.func(args))


if __name__ == "__main__":
    main()
