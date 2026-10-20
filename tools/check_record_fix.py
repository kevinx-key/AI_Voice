# check_record_fix.py —— `--record` 采样链路回归检查（不靠人按 F9）
#
# 背景：RING_SECONDS=3.0（src/audio.rs:22），而 run_record 原先只在松手后 drain 一次，
# 于是按住 5s 也只落盘「最后 3 秒」—— 噪声回归集样本全卡在 3.00s，
# <3s 的纯噪声甚至触发不了切段（flush 证据不足直接丢），回归集空转。
# 本脚本用 SendInput 合成按住 F9（复用 e2e 的 helper），断言落盘时长 ≈ 按住时长。
#
# 用法: python tools\check_record_fix.py [按住秒数，默认 5.0]
import glob
import os
import subprocess
import sys
import time
import wave

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_hotkey_suspend as e2e  # noqa: E402  （_send_key / tap_f9）

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(REPO, "target", "release", "listen.exe")


def main():
    hold = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
    running = subprocess.run(["powershell", "-NoProfile", "-Command",
                              "(Get-Process listen -ErrorAction SilentlyContinue).Count"],
                             capture_output=True, text=True).stdout.strip()
    if running not in ("", "0"):
        sys.exit("已有 listen.exe 在跑（%s 个）——先停掉，否则两个 F9 钩子并存" % running)

    out = os.path.join(REPO, ".cache", "_rec_check.wav")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    for old in glob.glob(os.path.join(os.path.dirname(out), "_rec_check*.wav")):
        os.remove(old)  # 上一次的残留会让断言失真

    p = subprocess.Popen([EXE, "--record", out], stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, text=True, encoding="utf-8",
                         errors="replace", bufsize=1)
    time.sleep(2.0)  # 等钩子装上 + 看到「按住…」提示
    print("合成按住 F9 %.1fs…" % hold)
    e2e.tap_f9(hold)
    for line in p.stdout:
        print("  · %s" % line.rstrip())
    p.wait()
    if not os.path.isfile(out):
        sys.exit("FAIL：没落盘（exit=%s）" % p.returncode)
    with wave.open(out) as w:
        secs = w.getnframes() / w.getframerate()
    lo, hi = hold + 0.15, hold + 0.8  # 松手后还有 300ms 尾音宽限
    ok = lo <= secs <= hi
    print("按住 %.2fs → 落盘 %.2fs（期望 %.2f~%.2fs）%s" % (hold, secs, lo, hi, "OK" if ok else "FAIL"))
    if abs(secs - 3.0) < 0.05:
        print("  ⚠️ 恰好 3.00s = 环形缓冲截断又回来了（RING_SECONDS 只在未取走时裁剪）")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
