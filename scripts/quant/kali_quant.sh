#!/usr/bin/env bash
# ============================================================================
# scripts/quant/kali_quant.sh —— Kali 侧的定量实验编排（E1 / E3 / E5 / E6）
#
# 为什么用脚本而不是一串命令行：每个实验的「口径」必须可复现（同一份脚本 = 同一口径），
# 报告里的每个数字都要能指回这里的一次 `bash kali_quant.sh <子命令>`。
#
# 子命令：
#   e3            对齐前后对照（96 MiB + --piece-conns 7，before/after 各 3 次，带 strace）
#   e1            E1 并发扫（256 MiB，conns 1/4/8/16/32/64 × 3，环路不限速）+ 两个地板
#   e1netem       E1 同扫，netem 400 mbit（跑完自动清 qdisc）
#   e6            预分配对比（1 GiB × conns 16：稀疏 set_len vs 先写零），filefrag 取 extent
#   e5            恒 500 场景（p1 / 重试时刻分布）
#
# 落地目录一律在 ext4（$WORK=~/quant-work）：/tmp 是 tmpfs（7.8 GB），
# 写到 tmpfs 上的「磁盘耗时」根本不是磁盘的 —— 这条在 net_faults 时期就踩过。
# ============================================================================
set -uo pipefail

BEFORE="$HOME/quant-bin/before"
AFTER="$HOME/quant-build/target-after/release/mc-downloader"
WORK="$HOME/quant-work"
RAW="$HOME/quant-raw"
FAKE="$HOME/net_faults/fakeorigin.py"
FIVE="$HOME/quant-src/always500.py"
TC=/usr/sbin/tc
PORT=8899
PORT5=8898

mkdir -p "$WORK" "$RAW"

csv_header() {  # csv_header <file> <header>
  [ -s "$1" ] || echo "$2" > "$1"
}

now() { date +%s.%N; }

origin_start() {  # origin_start <size_mb> <port>
  local mb="$1" port="$2"
  pkill -f "fakeorigin.py --size-mb $mb --port $port" 2>/dev/null
  sleep 0.3
  nohup python3 "$FAKE" --size-mb "$mb" --port "$port" > "$RAW/origin-$port.log" 2>&1 &
  for _ in $(seq 1 60); do
    grep -q '^SIZE=' "$RAW/origin-$port.log" 2>/dev/null && return 0
    sleep 0.2
  done
  echo "FAIL origin_start $mb/$port"; tail -3 "$RAW/origin-$port.log"; return 1
}

origin_stop() { pkill -f 'fakeorigin.py' 2>/dev/null; pkill -f 'always500.py' 2>/dev/null; sleep 0.2; }

netem_rate() {  # netem_rate <rate>|clear
  if [ "$1" = clear ]; then
    sudo -n $TC qdisc del dev lo root 2>/dev/null && echo "netem cleared"
  else
    sudo -n $TC qdisc replace dev lo root netem rate "$1" || { echo "FAIL netem $1"; return 1; }
    sudo -n $TC qdisc show dev lo | head -1
  fi
}

# 一次 fetch：清空落地目录 → 计时 → 记 CSV。$1=bin $2=size_mb $3=conns $4=timeout $5=retry $6=csv $7=label $8=tag
run_fetch() {
  local bin="$1" mb="$2" conns="$3" tmo="$4" retry="$5" csv="$6" label="$7" tag="$8"
  local name="q${mb}m.bin"
  rm -rf "$WORK"/* 2>/dev/null
  local t0 t1 rc bytes line mbps
  t0=$(now)
  "$bin" fetch "http://127.0.0.1:$PORT/$name" -d "$WORK" -n "$name" \
      --piece-conns "$conns" --timeout "$tmo" --retry "$retry" --force \
      > "$RAW/out-$label-$tag.log" 2> "$RAW/err-$label-$tag.log"
  rc=$?
  t1=$(now)
  bytes=$(stat -c %s "$WORK/$name" 2>/dev/null || echo 0)
  line=$(grep -h '分片：' "$RAW/out-$label-$tag.log" | head -1 | sed 's/.*分片：//')
  mbps=$(awk -v b="$bytes" -v s="$(awk -v a="$t0" -v b="$t1" 'BEGIN{print b-a}')" 'BEGIN{if(s>0)printf "%.2f", b/s/1048576; else print 0}')
  local secs; secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
  csv_header "$csv" "label,tag,conns,timeout,retry,bytes,secs,mbps,exit,piece_line"
  echo "$label,$tag,$conns,$tmo,$retry,$bytes,$secs,$mbps,$rc,\"$line\"" >> "$csv"
  echo "[$label/$tag] c=$conns ${secs}s ${mbps}MB/s exit=$rc bytes=$bytes :: $line"
}

# ---------------------------------------------------------------- e3
# 三臂（**次序即事实**：只有装出来的二进制能说话）：
#   legacy     = ~/quant-bin/before（2026-10-22 构建，肉眼可见的"改前"实物；
#                它的 piece 公式还是 4 MiB/16 MiB 固定档 —— 没有 size/conns）
#   before-mid = 由 5809715（对齐修复的父提交）重建，= 派发单里说的"改前"：
#                with_conns(size/conns, clamp[1,16]MiB) 但**不**取整到页
#   after      = 40b0051 构建（floor 到 4096）
e3() {
  local csv="$RAW/e3-align.csv"
  csv_header "$csv" "arm,round,conns,bytes,secs,mbps,exit,piece_line,strace"
  local MID="$HOME/quant-build/target-before-mid/release/mc-downloader"
  origin_start 96 "$PORT" || exit 1
  for pair in "legacy:$BEFORE" "before-mid:$MID" "after:$AFTER"; do
    local tag="${pair%%:*}" bin="${pair#*:}"
    if [ ! -x "$bin" ]; then echo "SKIP $tag（缺二进制 $bin）"; continue; fi
    for r in 1 2 3; do
      local name="q96m.bin"
      rm -rf "$WORK"/*
      local t0 t1 rc bytes line secs mbps
      # ⚠️ legacy 是 2026-10-22 的构建，CLI 里**还没有 --force**（那是 d8eb2f9 才加的）。
      # 给它补 --force 会直接 exit=2（clap 用法错误），所以这一条参数按二进制能力给；
      # 「目标目录清空」三臂都做，覆盖效果等价 —— 报告里如实写明这个差异。
      local force="--force"; [ "$tag" = legacy ] && force=""
      t0=$(now)
      strace -f -tt -ff -o "$RAW/e3f-$tag-r$r.strace" -e trace=lseek,write \
        "$bin" fetch "http://127.0.0.1:$PORT/$name" -d "$WORK" -n "$name" \
        --piece-conns 7 --timeout 30 --retry 3 $force \
        > "$RAW/out-e3-$tag-r$r.log" 2> "$RAW/err-e3-$tag-r$r.log"
      rc=$?; t1=$(now)
      bytes=$(stat -c %s "$WORK/$name" 2>/dev/null || echo 0)
      line=$(grep -h '分片：' "$RAW/out-e3-$tag-r$r.log" | head -1 | sed 's/.*分片：//')
      secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
      mbps=$(awk -v b="$bytes" -v s="$secs" 'BEGIN{if(s>0)printf "%.2f", b/s/1048576; else print 0}')
      echo "$tag,$r,7,$bytes,$secs,$mbps,$rc,\"$line\",e3-align-$tag-r$r.strace" >> "$csv"
      echo "[e3/$tag/r$r] ${secs}s ${mbps}MB/s exit=$rc :: $line"
    done
  done
  sha1sum "$WORK"/*.bin 2>/dev/null
  origin_stop
}

# ---------------------------------------------------------------- e1
e1_sweep() {  # e1_sweep <label> <csv>
  local label="$1" csv="$2"
  for c in 1 4 8 16 32 64; do
    for r in 1 2 3; do run_fetch "$AFTER" 256 "$c" 30 3 "$csv" "$label" "c${c}r${r}"; done
  done
}

e1() {
  local csv="$RAW/e1-kali.csv"
  csv_header "$csv" "label,tag,conns,timeout,retry,bytes,secs,mbps,exit,piece_line"
  origin_start 256 "$PORT" || exit 1
  # 地板①：ext4 顺序写（dd + fsync，256 MiB）
  for r in 1 2 3; do
    rm -f "$WORK/ddfloor.bin"; sync
    local t0 t1; t0=$(now)
    dd if=/dev/zero of="$WORK/ddfloor.bin" bs=1M count=256 conv=fsync status=none
    t1=$(now)
    local secs mbps; secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
    mbps=$(awk -v s="$secs" 'BEGIN{printf "%.2f", 256/s}')
    echo "floor-dd,$r,-,0,0,268435456,$secs,$mbps,0,\"dd bs=1M count=256 conv=fsync\"" >> "$csv"
    echo "[floor-dd/r$r] ${secs}s ${mbps}MB/s"
  done
  # 地板②：单连接、不落盘的源端吞吐（curl → /dev/null）
  for r in 1 2 3; do
    local t0 t1; t0=$(now)
    curl -s -o /dev/null "http://127.0.0.1:$PORT/q256m.bin"
    t1=$(now)
    local secs mbps; secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
    mbps=$(awk -v s="$secs" 'BEGIN{printf "%.2f", 256/s}')
    echo "floor-net,$r,1,0,0,268435456,$secs,$mbps,0,\"curl 1 conn -> /dev/null\"" >> "$csv"
    echo "[floor-net/r$r] ${secs}s ${mbps}MB/s"
  done
  e1_sweep e1-unlimited "$csv"
  origin_stop
}

e1netem() {
  local csv="$RAW/e1-kali.csv"
  csv_header "$csv" "label,tag,conns,timeout,retry,bytes,secs,mbps,exit,piece_line"
  origin_start 256 "$PORT" || exit 1
  netem_rate 400mbit || exit 1
  e1_sweep e1-netem400 "$csv"
  netem_rate clear
  origin_stop
}

# H 的来源：同一批 conns，另跑一轮带 strace 的（T 用上面那批**不带** strace 的，
# 因为 strace 会改调度 —— 两者混用等于把观测代价算进模型）。
e1hops() {
  local csv="$RAW/e1-hops.csv"
  origin_start 256 "$PORT" || exit 1
  for c in 1 4 8 16 32 64; do
    for r in 1 2 3; do
      local name="q256m.bin"
      rm -rf "$WORK"/*
      strace -f -tt -ff -o "$RAW/e1f-c$c-r$r.strace" -e trace=lseek,write \
        "$AFTER" fetch "http://127.0.0.1:$PORT/$name" -d "$WORK" -n "$name" \
        --piece-conns "$c" --timeout 30 --retry 3 --force \
        > "$RAW/out-e1hops-c${c}-r${r}.log" 2>&1
      echo "e1-hops,c${c}r${r},$c" >> "$csv"
      echo "[e1hops/c=$c/r$r] $(stat -c %s "$WORK/$name" 2>/dev/null || echo 0) bytes"
    done
  done
  origin_stop
}

# ---------------------------------------------------------------- e6
e6() {
  local csv="$RAW/e6-kali.csv"
  csv_header "$csv" "arm,round,conns,prealloc_secs,secs,mbps,bytes,extents,allocated,holes,exit"
  origin_start 1024 "$PORT" || exit 1
  for arm in sparse prealloc; do
    for r in 1 2; do
      local name="q1024m.bin" pre=0.0
      rm -rf "$WORK"/*
      if [ "$arm" = prealloc ]; then
        local p0 p1; p0=$(now)
        dd if=/dev/zero of="$WORK/$name.part" bs=1M count=1024 conv=fsync status=none
        p1=$(now)
        pre=$(awk -v a="$p0" -v b="$p1" 'BEGIN{printf "%.3f", b-a}')
      fi
      local t0 t1 rc bytes secs mbps
      t0=$(now)
      "$AFTER" fetch "http://127.0.0.1:$PORT/$name" -d "$WORK" -n "$name" \
        --piece-conns 16 --timeout 30 --retry 3 --force \
        > "$RAW/out-e6-$arm-r$r.log" 2> "$RAW/err-e6-$arm-r$r.log"
      rc=$?; t1=$(now)
      bytes=$(stat -c %s "$WORK/$name" 2>/dev/null || echo 0)
      secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
      mbps=$(awk -v b="$bytes" -v s="$secs" 'BEGIN{if(s>0)printf "%.2f", b/s/1048576; else print 0}')
      local ff; ff=$(filefrag -v "$WORK/$name" 2>/dev/null)
      local ext; ext=$(echo "$ff" | awk '/extents found/{print $1; exit}')
      local alloc; alloc=$(echo "$ff" | awk 'NR>3 && $1 ~ /^[0-9]+:/ && $4 != "unwritten" && $3 != "unknown" {n++} END{print n+0}')
      echo "$arm,$r,16,$pre,$secs,$mbps,$bytes,${ext:-0},${alloc:-0},0,$rc" >> "$csv"
      echo "$ff" > "$RAW/e6-filefrag-$arm-r$r.txt"
      echo "[e6/$arm/r$r] pre=${pre}s ${secs}s ${mbps}MB/s extents=${ext:-?} exit=$rc"
      # 追加耗时口径：预分配本身是额外的一遍写，报告里单独列
      echo "  (预分配额外 ${pre}s；合计 $(awk -v a="$pre" -v b="$secs" 'BEGIN{printf "%.3f", a+b}')s)"
    done
  done
  origin_stop
}

# ---------------------------------------------------------------- e5
e5() {
  local csv="$RAW/e5-retry.csv"
  csv_header "$csv" "arm,tag,conns,retry,declared,status,req_count,elapsed_s,exit,err_tail"
  origin_stop
  nohup python3 "$FIVE" --port "$PORT5" --log "$RAW/e5-server.log" > "$RAW/always500.log" 2>&1 &
  sleep 1
  : > "$RAW/e5-server.log"
  local url="http://127.0.0.1:$PORT5/f.bin"

  # A) --retry 0：单次尝试，测 p1（恒错源下 p1 必然 1.0，证据是「只发了 1 个请求」）
  for r in 1 2 3; do
    : > "$RAW/e5-server.log"
    local t0 t1 rc rc2 n secs
    t0=$(now)
    "$AFTER" fetch "$url" -d "$WORK" -n e5.bin -s 4194304 --piece-conns 1 --retry 0 --force \
      > "$RAW/out-e5-a$r.log" 2> "$RAW/err-e5-a$r.log"
    rc=$?; t1=$(now)
    secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
    n=$(grep -c 'ev=req' "$RAW/e5-server.log")
    cp "$RAW/e5-server.log" "$RAW/e5-server-a$r.log"
    echo "retry0,a$r,1,0,4194304,500,$n,$secs,$rc,\"$(tail -1 "$RAW/err-e5-a$r.log" | cut -c1-90)\"" >> "$csv"
    echo "[e5/retry0/a$r] ${secs}s req=$n exit=$rc"
  done

  # B) 分片层面重试时刻分布：64 MiB / 4 连接 = 4 个并发分片全挂，看 4 份重试是否同刻
  : > "$RAW/e5-server.log"
  local t0 t1 rc secs
  t0=$(now)
  "$AFTER" fetch "$url" -d "$WORK" -n e5b.bin -s 67108864 --piece-conns 4 --retry 3 --force \
    > "$RAW/out-e5-b.log" 2> "$RAW/err-e5-b.log"
  rc=$?; t1=$(now)
  secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')
  local n; n=$(grep -c 'ev=req' "$RAW/e5-server.log")
  cp "$RAW/e5-server.log" "$RAW/e5-server-b.log"
  echo "retry3,b1,4,3,67108864,500,$n,$secs,$rc,\"$(tail -1 "$RAW/err-e5-b.log" | cut -c1-90)\"" >> "$csv"
  echo "[e5/retry3/b1] ${secs}s req=$n exit=$rc  (期望 4×(1+3)=16)"
  origin_stop
}

case "${1:-}" in
  e3) e3 ;;
  e1) e1 ;;
  e1netem) e1netem ;;
  e1hops) e1hops ;;
  e6) e6 ;;
  e5) e5 ;;
  *) echo "usage: $0 {e3|e1|e1netem|e1hops|e6|e5}"; exit 2 ;;
esac
