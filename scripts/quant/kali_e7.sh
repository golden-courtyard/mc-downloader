#!/usr/bin/env bash
# ============================================================================
# scripts/quant/kali_e7.sh —— E7「回 200 回退的代价」（#6 的被测行为）
#
# 口径（先写死，测完不改）：
#   源：scripts/quant/rangeserv.py 的三种模式
#       A = 正常 206（Range 被尊重）
#       B = --ignore-range：收到 Range 也回 200 + 整份 body（模拟"不认 Range 的站"）
#       C = --no-length：chunked、不给 Content-Length ⇒ **从一开始就走 one_stream**
#           （C 不是被测项，是**对照组**：B 降级后走的就是这条单流路径，没有 C 就分不开
#            "单流本身的代价" 与 "回退的代价"）
#   条件：1 GiB、--piece-conns 4、每模式 3 轮；落盘 ext4（~/quant-work），**不进 /tmp**
#   量：墙钟 / 源端请求数（按 method、has_range、status 分类）/ 源端实际下发字节 /
#       客户端回退行是否出现 / 落地 SHA1
#   判据（写死）：B 相对 A 只多 1 个被拒的 Range 请求、0 额外下发字节、墙钟差 ≤ 1% 或 ≤ 1 RTT；
#         超出即报「降级路径有问题」，不写成"正常波动"。
#
# 设计要点：
#   ① **交错跑**（每轮 A/B/A-c1/B-c1/C 各一次）：页缓存与写回状态的漂移摊到各臂上，
#      而不是让"最后跑的那一臂"独自承担（第一版按臂串行跑，B-c1 被拖到 7.8 s，就是这种漂移）。
#   ② 每轮前 `sync`：把上一轮 1 GiB 脏页刷下去再计时，否则测的是写回队列。
#   ③ A-c1 / B-c1 是**对照臂**不是改口径：--piece-conns 1 时只有 1 个 Range 请求在途，
#      也就是判据里"只多 1 个"的原型。
#
# 跑法：bash kali_e7.sh
# ============================================================================
set -uo pipefail

BIN="$HOME/quant-build/target-e7/release/mc-downloader"
SRV="$HOME/quant-src/rangeserv.py"
SRC="$HOME/quant-src/q1024m.bin"          # 1 GiB，内容规则 bytes(i%251)
WORK="$HOME/quant-work"
RAW="$HOME/quant-raw"
PORT=8899
EXPECT=e725338708d1ba48048f65b3fb38798461bdbf27
CSV="$RAW/e7-fallback.csv"

[ -x "$BIN" ] || { echo "缺二进制: $BIN"; exit 1; }
[ -f "$SRC" ] || { echo "缺源文件: $SRC"; exit 1; }
[ "$(stat -c %s "$SRC")" = 1073741824 ] || { echo "源文件不是 1 GiB"; exit 1; }
mkdir -p "$RAW" "$WORK"

echo "arm,mode,conns,round,wall_s,landed_bytes,sha1_ok,fallback_line,reqs,heads,range_reqs,resp200,resp206,aborted,sent_total,sent_on_range_reqs,server_secs" > "$CSV"

origin_start() {  # origin_start <log> <mode: A|B|C>
  pkill -f 'rangeserv.py' 2>/dev/null; sleep 0.2
  local args=(--file "$SRC" --port "$PORT" --log "$1")
  [ "$2" = "B" ] && args+=(--ignore-range)
  [ "$2" = "C" ] && args+=(--no-length)
  nohup python3 "$SRV" "${args[@]}" > "$RAW/e7-origin-$$.out" 2>&1 &
  for _ in $(seq 1 80); do
    grep -q '^READY' "$RAW/e7-origin-$$.out" 2>/dev/null && return 0
    sleep 0.1
  done
  echo "FAIL origin_start"; tail -3 "$RAW/e7-origin-$$.out"; return 1
}

run_one() {  # run_one <arm> <mode> <conns> <round>
  local arm="$1" mode="$2" conns="$3" r="$4"
  local log="$RAW/e7-$arm-r$r.log" out="$RAW/e7-$arm-r$r.out" err="$RAW/e7-$arm-r$r.err"
  : > "$log"
  origin_start "$log" "$mode" || return 1
  sync; rm -rf "$WORK"/* 2>/dev/null; sync
  local t0 t1 rc bytes sha ok fb wall stats
  t0=$(date +%s.%N)
  "$BIN" fetch "http://127.0.0.1:$PORT/q1024m.bin" -d "$WORK" -n q1g.bin \
    --piece-conns "$conns" --timeout 30 --retry 3 --force > "$out" 2> "$err"
  rc=$?
  t1=$(date +%s.%N)
  sync
  pkill -f 'rangeserv.py' 2>/dev/null; sleep 0.15

  bytes=$(stat -c %s "$WORK/q1g.bin" 2>/dev/null || echo 0)
  sha=$(sha1sum "$WORK/q1g.bin" 2>/dev/null | cut -d' ' -f1)
  ok=0; [ "$sha" = "$EXPECT" ] && ok=1
  fb=0; grep -q '已退回单流下载' "$out" && fb=1
  wall=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')

  # 源端日志归类：ev=req 给请求数与类型；ev=done|broken 给响应数、实际下发字节、服务端耗时
  stats=$(awk '
    /ev=req/ {
      for (i=1;i<=NF;i++) { split($i,kv,"="); v[kv[1]]=kv[2] }
      reqs++; if (v["method"]=="HEAD") heads++; if (v["has_range"]==1) rangereq++
    }
    /ev=(done|broken)/ {
      for (i=1;i<=NF;i++) { split($i,kv,"="); v[kv[1]]=kv[2] }
      if ($0 ~ /ev=broken/) aborted++
      if (v["status"]==200) r200++; if (v["status"]==206) r206++
      sent += v["sent"]
      if (v["has_range"]==1) sentr += v["sent"]
      secs += v["secs"]
    }
    END { printf "%d,%d,%d,%d,%d,%d,%d,%d,%.3f", reqs+0, heads+0, rangereq+0, r200+0, r206+0, aborted+0, sent+0, sentr+0, secs+0 }
  ' "$log")
  echo "$arm,$mode,$conns,$r,$wall,$bytes,$ok,$fb,$stats" >> "$CSV"
  echo "[$arm/r$r] wall=${wall}s sha_ok=$ok fb_line=$fb server_secs=$(echo "$stats" | cut -d, -f9) sent=$(echo "$stats" | cut -d, -f7)"
}

# 交错跑三轮：每轮把五个臂各跑一次
for r in 1 2 3; do
  run_one A-c4 A 4 "$r"
  run_one B-c4 B 4 "$r"
  run_one A-c1 A 1 "$r"
  run_one B-c1 B 1 "$r"
  run_one C-stream C 4 "$r"
done

echo "--- CSV ---"; cat "$CSV"
