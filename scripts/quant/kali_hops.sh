#!/usr/bin/env bash
# ============================================================================
# scripts/quant/kali_hops.sh —— #8 的 before 数据：1 GiB × --piece-conns 1/4/16 的 H
#
# 口径（先写死，测前不改）：
#   文件 1 GiB（内容规则 bytes(i%251)，源 ~/quant-src/q1024m.bin，与 E4/E7 同一份）
#   落盘 **ext4**（~/quant-work；/tmp 是 tmpfs，磁盘行为完全不同）
#   每档 3 轮，三轮都跑 strace（H 是确定性的计数，三轮用来确认没有偶发）
#   量：H（写跳变次数，口径见 hops_write.py，继承 hops.py）、片数、片大小、写次数、下发字节
#   判据：conns=1 ⇒ H == 0；conns≥4 ⇒ H ≈ 片数-1（1 GiB/16 MiB ⇒ 64 片，期望 ≈63）
#
# 为什么用 `-ff -y`：
#   -y  必须：分片每片各自开句柄，只有 fd→路径装饰能把它们归到**同一个文件**上；
#        没有 -y 只能按 fd 分组，会得到 n 条各自单调的序列、H 假性为 0。
#   -ff 为了不被 `<unfinished ...>` / `<... resumed>` 拆行（单文件 -o 在 -f 下会拆，
#        hops_write.py 虽然能合并，但 -ff 直接从源头避免）。
#
# 调用（after 也用同一条命令，把 BIN 换成新构建即可）：
#   bash kali_hops.sh [BIN]
# 产出：~/quant-raw/hops-write.csv → 取回 scripts/quant/raw/hops-write.csv
# ============================================================================
set -uo pipefail

BIN="${1:-$HOME/quant-build/target-e7/release/mc-downloader}"
SRV="$HOME/quant-src/rangeserv.py"
SRC="$HOME/quant-src/q1024m.bin"
WORK="$HOME/quant-work"
RAW="$HOME/quant-raw"
PORT=8899
CSV="$RAW/hops-write.csv"
HOPS="$HOME/quant-src/hops_write.py"

[ -x "$BIN" ] || { echo "缺二进制: $BIN"; exit 1; }
[ -f "$SRC" ] || { echo "缺源文件: $SRC"; exit 1; }
[ -f "$HOPS" ] || { echo "缺脚本: $HOPS"; exit 1; }
mkdir -p "$RAW" "$WORK"
[ -s "$CSV" ] || echo "run,conns,piece_size,pieces,H,total_bytes,files,writes,distinct_starts,hops_back,hops_forward_gap,target,degraded_no_y,partial_writes,first_offsets,H_over_pieces_minus_1" > "$CSV"

start_origin() {  # start_origin <log>
  pkill -f 'rangeserv.py' 2>/dev/null; sleep 0.2
  nohup python3 "$SRV" --file "$SRC" --port "$PORT" --log "$1" > "$RAW/hops-origin.out" 2>&1 &
  for _ in $(seq 1 80); do grep -q '^READY' "$RAW/hops-origin.out" 2>/dev/null && break; sleep 0.1; done
}

for conns in 1 4 16; do
  for r in 1 2 3; do
    olog="$RAW/hops-origin-c$conns-r$r.log"
    start_origin "$olog"
    out="$RAW/hops-c$conns-r$r.out"; pre="$RAW/hopsf-c$conns-r$r.strace"
    rm -f "$pre".* 2>/dev/null
    rm -rf "$WORK"/* 2>/dev/null; sync
    strace -f -tt -ff -y -o "$pre" -e trace=lseek,write,pwrite64 \
      "$BIN" fetch "http://127.0.0.1:$PORT/q1024m.bin" -d "$WORK" -n q1g.bin \
      --piece-conns "$conns" --timeout 30 --retry 3 --force > "$out" 2>&1
    rc=$?
    sync
    pieces=$(grep -o '分片：[0-9]* 片' "$out" | head -1 | grep -o '[0-9]*')
    psize=$(grep -o '片大小 [0-9]* 字节' "$out" | head -1 | grep -o '[0-9]*')
    byte=$(stat -c %s "$WORK/q1g.bin" 2>/dev/null || echo 0)
    echo "  [c$conns/r$r] rc=$rc bytes=$byte pieces=${pieces:-?} piece_size=${psize:-?}"
    python3 "$HOPS" "$pre" --path-substr q1g.bin --run "c$conns-r$r" \
      --conns "$conns" --piece-size "${psize:-0}" --pieces "${pieces:-0}" --csv "$CSV" \
      --order-out "$RAW/order-c$conns-r$r.csv"
    python3 "$HOME/quant-src/crosscheck_order.py" "$RAW/order-c$conns-r$r.csv" "$olog"
  done
done

pkill -f 'rangeserv.py' 2>/dev/null
echo "--- $CSV ---"; cat "$CSV"
