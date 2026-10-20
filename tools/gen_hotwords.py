#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 / 更新 `resources\\hotwords\\keyboard.tsv`（倾听输入法「键圈」热词包）。

用法::

    python tools\\gen_hotwords.py             # 报告 + 写文件
    python tools\\gen_hotwords.py --dry-run   # 只报告，不写文件

数据来源（`docs\\键圈术语资料包`）:

* `键圈术语库.json`  223 条结构化术语（en/abbr/full/cn/category/definition/significance）
* `键圈热词榜.md`    GeekHack 11,875 篇真实帖子词频，用来给词条打权重

输出格式（UTF-8 / LF / 制表符分隔 / 5 列）::

    词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明

* **编码留空** = 只用于录音侧（ASR 热词偏置 + 术语保留），不生成 Rime 候选
* **权重 0~100** = 优先级，决定进 FunASR-Nano prompt 的前 N 条（KV 上限 30）

编码冲突判定：把编码当全拼输入去雾凇自带词库（`resources\\rime\\schemes\\rime-ice\\cn_dicts\\*.dict.yaml`）
里查；命中高频词（权重 ≥ CONFLICT_W）说明这个编码本来就能打出别的东西（如 `sa` → 撒 140910），
英文词条就不再置顶候选（编码置空，仍进录音侧）。

本脚本只做「一次生成 + 复核」；词表的日常增删改在 GUI（热词管理器）或直接编辑 tsv 里做。
"""

import argparse
import json
import math
import os
import re
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PKG = os.path.join(ROOT, "docs", "键圈术语资料包")
JSON_FILE = os.path.join(PKG, "键圈术语库.json")
HOT_FILE = os.path.join(PKG, "键圈热词榜.md")
DICT_DIR = os.path.join(ROOT, "resources", "rime", "schemes", "rime-ice", "cn_dicts")
OUT_DIR = os.path.join(ROOT, "resources", "hotwords")
OUT_FILE = os.path.join(OUT_DIR, "keyboard.tsv")

# 词库里权重 ≥ 此值算「这个编码是有主的」→ 英文词条不置顶候选
CONFLICT_W = 1000

# 无词频数据时按类别给的默认权重（0~100）
CAT_W = {
    "中文黑话": 62,
    "行业机制": 55,
    "配列": 54,
    "键帽": 50,
    "轴体": 50,
    "结构": 50,
    "固件": 45,
    "声音": 45,
    "社区": 36,
    "品牌": 32,
}

# ---------------------------------------------------------------- 人工覆写表

# 1) 中文黑话（JSON 里 abbr 是中文，没拼音）→ 手写词条与全拼编码
BLACK_TALK = {
    "客制化": [("客制化", "kezhihua")],
    "键圈": [("键圈", "jianquan")],
    "开团": [("开团", "kaituan")],
    "发车 / 开车": [("发车", "fache"), ("开车", "kaiche")],
    "上车": [("上车", "shangche")],
    "下车": [("下车", "xiache")],
    "车价": [("车价", "chejia")],
    "现货车": [("现货车", "xianhuoche")],
    "拼车": [("拼车", "pinche")],
    "团长": [("团长", "tuanzhang")],
    "跑路": [("跑路", "paolu")],
    "冲": [("冲", "")],  # 单字通用词，置顶没意义 → 只进录音侧
    "真香": [("真香", "zhenxiang")],
    "入坑": [("入坑", "rukeng")],
    "烧 / 退烧": [("退烧", "tuishao")],
    "氪金": [("氪金", "kejin")],
    "萌新 / 大佬": [("萌新", "mengxin"), ("大佬", "dalao")],
    "开箱": [("开箱", "kaixiang")],
    "评测 / 测评": [("评测", "pingce"), ("测评", "ceping")],
    "代组": [("代组", "daizu")],
    "套件": [("套件", "taojian")],
    "热插拔": [("热插拔", "rechaba")],
    "焊轴 / 焊轴工": [("焊轴", "hanzhou")],
    "拔轴器": [("拔轴器", "bazhouqi")],
    "吸锡": [("吸锡", "")],  # xixi 是常用拼音（嘻嘻）→ 只进录音侧
    "卫星轴": [("卫星轴", "weixingzhou")],
    "静电容": [("静电容", "jingdianrong")],
    "键帽卡位": [("卡位", "kawei"), ("菊孔", "jukong")],
    "打油": [("打油", "dayou")],
    "外设天下": [("外设天下", "waishetianxia")],
    "什么值得买": [("什么值得买", "shenmezhidemai")],
    "键盘吧": [("键盘吧", "jianpanba")],
}

# 2) 带连字符/空格的词条：默认不给编码，这几个常用词人工补上
EXTRA_CODES = {
    "B-stock": "bstock",
    "In-stock": "instock",
    "Dye-sub": "dyesub",
    "Topre / EC": "topre",
    "Hall Effect / HE": "hall",
    "MDA / ASA": "mda",
    "DSS / DCS": "dss",
    "KAT / KAM": "kat",
}

# 3) 「键圈热词榜」里有、但 JSON 术语库里缺的（按词频给权重）
HOTWORD_ONLY = {
    "SSK": ("复古键盘", "IBM Space Saving Keyboard，紧凑版 Model M"),
    "AEK": ("复古键盘", "Apple Extended Keyboard"),
    "G80": ("复古键盘", "Cherry G80 系列"),
    "G81": ("复古键盘", "Cherry G81 系列"),
    "XT": ("复古键盘", "IBM PC/XT 键盘"),
    "LZ": ("品牌", "LZ 客制化键盘（韩国）"),
    "TGR": ("品牌", "TGR 客制化键盘（韩国）"),
    "KMAC": ("键盘型号", "KMAC 经典客制化键盘"),
    "GH60": ("键盘型号", "GH60 60% 开源 PCB"),
    "OTD": ("品牌", "OTD 韩国客制化键盘"),
    "C225": ("品牌", "客制化品牌/型号"),
    "FC660C": ("键盘型号", "Leopold FC660C 静电容"),
    "QFR": ("键盘型号", "CM QuickFire Rapid"),
    "CLS": ("键盘型号", "CLS 客制化键盘"),
    "WASD": ("键帽", "WASD 键帽定制商 / 游戏键位"),
    "OG": ("社区", "Original Gangster，原版/初代"),
    "FE": ("社区", "Founder's Edition，创始人版"),
    "WTB": ("交易", "Want To Buy，求购"),
    "WTS": ("交易", "Want To Sell，出售"),
    "WTT": ("交易", "Want To Trade，交换"),
    "R1": ("团购机制", "Round 1，第一期团"),
    "R2": ("团购机制", "Round 2，第二期/复刻"),
    "R3": ("团购机制", "Round 3，第三期团"),
    "R4": ("团购机制", "Round 4，第四期团"),
    "R5": ("团购机制", "Round 5，第五期团"),
}

# 4) 明确不要的词条（缩写太泛/意义不明/会误导）
SKIP = {"EC", "HE", "ASA", "KAM", "DCS", "MK", "DIY", "MECHANICALKEYBOARDS"}

# 纯尺寸/百分比/单字母垃圾 token（100% / 7u / 6.25u / 1800 / r）
JUNK_TOKEN = re.compile(r"^([\d.%u]+|[a-z%])$")

CJK = re.compile(r"[\u4e00-\u9fff]")
# 编码只能用小写字母：雾凇全拼 speller/alphabet 只有 a-zA-Z，数字/符号进不了输入缓冲
CLEAN_CODE = re.compile(r"^[a-z]{2,12}$")
# 含数字/符号的缩写（R1/G80/WS2812/nRF52832…）→ 只进录音侧；这些词在英文模式下直接敲即可
CODE_STRIP = re.compile(r"[^A-Za-z0-9]")
# 缩写形态：GB (Group Buy) / MOQ (Minimum Order Quantity)
ABBR_FORM = re.compile(r"^([A-Za-z0-9.+-]+)\s*\(")


def load_json_terms():
    with open(JSON_FILE, encoding="utf-8") as f:
        return json.load(f)["terms"]


def load_freq():
    """从热词榜 md 里抠出 {词: 词频}。"""
    freq = {}
    with open(HOT_FILE, encoding="utf-8") as f:
        for line in f:
            m = re.match(r"^\|\s*\d+\s*\|\s*\*\*(.+?)\*\*\s*\|\s*([\d,]+)\s*\|", line)
            if m:
                word = m.group(1).strip()
                freq[word] = int(m.group(2).replace(",", ""))
    return freq


def load_dict_codes():
    """扫雾凇词库 → {无空格编码: (词, 权重)}，只留高频。"""
    best = {}
    if not os.path.isdir(DICT_DIR):
        print(f"!! 找不到词库目录，跳过错音冲突检查: {DICT_DIR}", file=sys.stderr)
        return best
    for name in sorted(os.listdir(DICT_DIR)):
        if not name.endswith(".dict.yaml"):
            continue
        with open(os.path.join(DICT_DIR, name), encoding="utf-8") as f:
            for line in f:
                if line.startswith("#") or "\t" not in line:
                    continue
                parts = line.rstrip("\n").split("\t")
                if len(parts) < 2 or not CJK.search(parts[0]):
                    continue
                code = parts[1].replace(" ", "")
                try:
                    w = int(parts[2]) if len(parts) > 2 else 1
                except ValueError:
                    w = 1
                if w < CONFLICT_W:
                    continue
                if code not in best or best[code][1] < w:
                    best[code] = (parts[0], w)
    return best


def weight_of(word, freq, cat):
    """热词榜有词频 → 对数归一化到 20~95；否则按类别默认。"""
    if word in freq:
        return max(20, min(95, round(20 * math.log10(freq[word] + 1))))
    return CAT_W.get(cat, 40)


def split_alternatives(s):
    return [x.strip() for x in re.split(r"\s*/\s*", s) if x.strip()]


def build(keep_phrases=False):
    terms = load_json_terms()
    freq = load_freq()
    entries = []  # (词条, 编码, 类别, 权重, 说明)
    stats = defaultdict(int)

    for t in terms:
        cat = t["category"]
        abbr = (t["abbr"] or "").strip()
        en = (t["en"] or "").strip()
        cn = (t["cn"] or "").strip().replace("/", "／")
        desc = (t.get("definition") or "").strip()
        desc = re.sub(r"\s+", " ", desc)
        if len(desc) > 60:
            desc = desc[:59] + "…"

        # 中文黑话：手写表
        if abbr in BLACK_TALK:
            for text, code in BLACK_TALK[abbr]:
                entries.append((text, code, cat, weight_of(text, freq, cat), cn))
                stats["黑话"] += 1
            continue

        # 英文：单 token（缩写或单词）才有编码
        tokens = split_alternatives(abbr) or [abbr]
        for tok in tokens:
            tok = tok.strip()
            if not tok:
                continue
            m = ABBR_FORM.match(tok)
            text = m.group(1) if m else tok
            text = text.strip()
            if not text or JUNK_TOKEN.match(text.lower()) or text.upper() in SKIP:
                stats["跳过"] += 1
                continue
            w = weight_of(text.upper() if text.isupper() else text, freq, cat)
            if text in EXTRA_CODES:
                entries.append((text, EXTRA_CODES[text], cat, w, cn))
                stats["覆写"] += 1
                continue
            if "/" in text or " " in text:
                # 多词/带空格：默认丢（说不出口的英文短语，占热词额度）
                if keep_phrases:
                    entries.append((text, "", cat, w, cn))
                stats["丢短语"] += 1
                continue
            code = CODE_STRIP.sub("", text).lower()
            if not CLEAN_CODE.match(code):
                # 单 token 但编码不可用（含数字：6KRO/G80/WS2812）；这类词英文模式直接敲 → 只进录音侧
                entries.append((text, "", cat, w, cn))
                stats["仅录音"] += 1
                continue
            entries.append((text, code, cat, w, cn))
            stats["带编码"] += 1

    # 热词榜补充项（已在 JSON 里的不再重复加）
    seen_text = {e[0].lower() for e in entries}
    for word, (cat, desc) in HOTWORD_ONLY.items():
        if word.lower() in seen_text:
            continue
        w = weight_of(word, freq, cat)
        entries.append((word, word.lower(), cat, w, desc))
        stats["热词榜补充"] += 1

    # 去重（词条+编码），保留权重最高
    dedup = {}
    for e in entries:
        key = (e[0], e[1])
        if key not in dedup or dedup[key][3] < e[3]:
            dedup[key] = e
    dedup.update({k: v for k, v in dedup.items()})
    return sorted(dedup.values(), key=lambda e: (-e[3], e[0].lower())), stats


def apply_conflicts(entries, dict_codes):
    """英文词条的编码若在词库里被高频占用 → 去掉编码（保留词条）。

    另：编码含数字的（R1/G80/WS2812/nRF52832）一律降级为仅录音 —— 全拼字母表只有 a-z，
    这类词在中文态压根敲不出来（英文模式下直接输入即可）。
    """
    out, conflicts = [], []
    for text, code, cat, w, desc in entries:
        if code and not re.match(r"^[a-z]+$", code):
            out.append((text, "", cat, w, desc))
            continue
        if code and not CJK.search(text) and code in dict_codes:
            word, dw = dict_codes[code]
            conflicts.append((text, code, word, dw))
            out.append((text, "", cat, w, f"{desc}（{code} 已被「{word}」占用）" if desc else f"{code} 已被「{word}」占用"))
        else:
            out.append((text, code, cat, w, desc))
    return out, conflicts


HEADER = """# 倾听输入法 · 热词包：键圈（机械键盘社区）
# 格式：词条 <TAB> 编码 <TAB> 类别 <TAB> 权重 <TAB> 说明
#   编码只能用小写字母（全拼字母表只有 a-z，数字/符号打不出来 → 含数字的词条自动降级为仅录音）
#   编码留空 = 只用于录音侧（ASR 热词偏置 / 术语保留），不生成打字候选
#   权重 0~100 = 优先级，越大越先被送进识别模型的热词 prompt
# 生成：python tools/gen_hotwords.py（源：docs/键圈术语资料包）
# 可直接编辑（GUI「热词」页读写的就是这个文件）"""


def write_tsv(entries, path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    lines = [HEADER]
    for text, code, cat, w, desc in entries:
        lines.append(f"{text}\t{code}\t{cat}\t{w}\t{desc}")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true", help="只报告不写文件")
    ap.add_argument("--keep-phrases", action="store_true", help="多词英文短语也收（只进录音侧）")
    args = ap.parse_args()

    print("扫雾凇词库（错音冲突检查）…")
    dict_codes = load_dict_codes()
    print(f"  高频编码 {len(dict_codes)} 条")

    entries, drop_stats = build(args.keep_phrases)
    entries, conflicts = apply_conflicts(entries, dict_codes)

    withcode = [e for e in entries if e[1]]
    print(f"\n词条合计 {len(entries)} 条；带打字编码 {len(withcode)} 条；仅录音 {len(entries) - len(withcode)} 条")
    bycat = defaultdict(int)
    for e in entries:
        bycat[e[2]] += 1
    print("按类别：" + "；".join(f"{k} {v}" for k, v in sorted(bycat.items(), key=lambda x: -x[1])))
    print("统计：" + "；".join(f"{k} {v}" for k, v in sorted(drop_stats.items())))
    if conflicts:
        print(f"\n编码冲突 → 只进录音侧（{len(conflicts)} 条）：")
        for text, code, word, dw in conflicts:
            print(f"  {text:<14}{code:<12} 词库占用：{word}({dw})")

    print(f"\n进 FunASR prompt 的前 30 条预览：")
    for e in entries[:30]:
        print(f"  {e[3]:>3}  {e[0]:<14}{e[1]:<12}{e[2]}")

    if args.dry_run:
        print("\n--dry-run：未写文件")
        return
    write_tsv(entries, OUT_FILE)
    print(f"\n已写 {OUT_FILE}")


if __name__ == "__main__":
    main()
