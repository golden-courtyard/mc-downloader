#!/usr/bin/env bash
# ============================================================================
# scripts/net_faults/lib.sh —— net_faults 故障测试框架核心库
#
# 用法：场景脚本里 `source "$(dirname "$0")/../lib.sh"`，然后 load_env / apply / run_case。
#
# 接口清单（design §2.2 rev3，唯一真相源；该表共 12 行，本文件实现其中 10 个 shell 函数）：
#   load_env
#   start_origin <size_mb> <期望sha1>
#   apply        <netem 参数...>
#   clear
#   run_case     <name> <断言函数> [参数...] [-- <断言函数> [参数...]]...
#   assert_rate  <实测bps> <目标bps> [容差]        # 容差默认 0.2
#   assert_sha   <文件> <期望sha1>
#   assert_local_only [pid]                        # 不传 pid 用 CASE_PID 的采样证据；
#                                                  # 显式传别的 pid 时一律现场 ss 采（不借用）
#   assert_rc    <期望rc|nonzero>                  # 读 CASE_RC
#   emit_report  <输出路径>
#
# 内部函数（**不是**契约，场景不要直接调）：
#   self_check                    只读体检，不删除任何东西（tasks T0.3）
#   __nf_pid_alive <pid>          判活：按 /proc/<pid>/stat 的 state ≠ Z
#                                 （**不用 `kill -0`** —— 它对僵尸返回成功，会让等待循环空转）
#
# 契约基线：design §2.2 rev3（2026-10-21 回填：run_case 注入约定 / clear 自检判据 / fakeorigin CLI）
#
# 判定行格式（机器可解析，带宽一律 bps 整数）：
#   PASS l2_rate_8mbit rc=0 rate=982000 expect=1000000 tol=0.2 sha=ok
#   FAIL l2p_loss_20 rc=137 timeout=120s part_state=present(expect=absent)
#   `rc=` 恒定存在；`timeout=` 只在超时时出现；其余是各断言给出的证据键。
#
# ⚠ 使用纪律：**场景要判 PASS 必须自己显式 `clear`**。
#   EXIT trap 里的 clear 只是兜底 —— 它静默删除、但自检失败会打到 stderr
#   并落一个 $OUT/logs/trap_self_check.fail 标记；它不能替代场景内的显式清理，
#   也不能为本次判定兜底（残留会污染下一条场景）。
#
# 两个必须在框架层成对管理的坑（design §1.3 rev3 实捕）：
#   1) netem 没有 mtu 参数 → 改接口 MTU：apply 设 1500，clear 必须还原 65536
#   2) iptables -D 必须带完整 spec（含 --reject-with tcp-reset），漏了就删不掉、规则残留
#
# 本文件被 source，不设置 shell 选项（set -e 由调用方决定）。
# ============================================================================

[[ -n ${__NF_LIB_LOADED:-} ]] && return 0
__NF_LIB_LOADED=1

__NF_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

# sudoers 的 NOPASSWD 是按「完整路径」匹配的（见 tasks T2.2），所以用绝对路径。
TC_BIN=${TC_BIN:-/usr/sbin/tc}
IP_BIN=${IP_BIN:-/usr/sbin/ip}
IPT_BIN=${IPT_BIN:-/usr/sbin/iptables}

NF_MTU_FAULT=${NF_MTU_FAULT:-1500}
NF_MTU_CLEAN=${NF_MTU_CLEAN:-65536}

SUDO=()
# -n：sudoers 一旦失配（改错文件 / 重装 / 路径变成 /sbin/tc），sudo 会在 tty 上等密码，
# 而 trap 里的 stderr 已被重定向 —— 没有 -n 就是一次无提示挂起。
if [[ ${EUID:-$(id -u)} -ne 0 ]]; then SUDO=(sudo -n); fi

# ---- run_case 与被测命令之间传递状态用的全局（场景可覆盖，见 load_env 注释） ----
CASE_CMD=()          # 数组：$BIN 之后的命令行参数
CASE_OUT=""          # 期望落盘的文件；给了才算吞吐、才做 sha1
CASE_TIMEOUT_S=120   # 单次被测命令超时
VERDICTS=()          # 本次进程内所有判定行
ORIGIN_PID=""
CASE_PID=""
CASE_PEERS=""
CASE_PEERS_ERR=""    # 采样期 ss 的 stderr（没装 iproute2 / 无权限时用来说明原因）
CASE_PEERS_PID=""
CASE_RC=0
CASE_ELAPSED_MS=0
CASE_BYTES=0
CASE_RATE_BPS=0
CASE_OUT_MTIME=""    # 本轮 CASE_OUT 的 mtime（秒）；用来判 stale
CASE_TIMEOUT=0
CASE_STALE=0         # CASE_OUT 的 mtime < 起跑时刻 → 是上一轮残留，不是本轮产物
ASSERT_DETAIL=""     # 断言函数写这里，run_case 拼进判定行
__NF_CHILDREN=()     # 所有还在跑的子进程（被测进程 / 采样器），trap 统一回收
# 本 shell 有没有真的「动过手」：load_env / apply 置 1。
# 判据不能用 IFACE 非空 —— 环境里带着 IFACE=eth0 却没 load_env 时，
# 门会放行 → trap 去改桥接网卡 eth0 的 MTU（IMPL-7）。
__NF_ARMED=0
__NF_ERRLOG="$(mktemp -t netfaults.XXXXXX 2>/dev/null || echo /tmp/netfaults.err)"

# ============================================================================
# load_env —— 默认值宿；已存在的环境变量不覆盖（场景可提前设定）
# ============================================================================
load_env() {
    export IFACE="${IFACE:-lo}"
    export PORT="${PORT:-8899}"
    export SIZE_MB="${SIZE_MB:-16}"
    export BIN="${BIN:-$(cd -- "$__NF_DIR/../.." && pwd)/target/release/mc-downloader}"
    export OUT="${OUT:-/tmp/net_faults}"
    export ORIGIN="${ORIGIN:-$__NF_DIR/fakeorigin.py}"
    export VERDICT_FILE="${VERDICT_FILE:-$OUT/verdicts.log}"

    mkdir -p -- "$OUT" || { echo "load_env: 建不了 OUT=$OUT" >&2; return 1; }
    mkdir -p -- "$OUT/logs" || return 1
    # 注意：不截断 VERDICT_FILE —— 清空是发起方（run_all.sh / 人或 rm）的责任，
    # 否则多场景同 OUT 会互相清掉别人的判定行。

    [[ "$IFACE" == "lo" ]] || echo "load_env: 警告 IFACE=$IFACE（默认 lo）" >&2
    __NF_ARMED=1
    return 0
}

# ============================================================================
# start_origin —— 起假源，等端口就绪，导出 ORIGIN_PID
#   $1 size_mb   $2 期望 sha1（空串则跳过校验）
#   契约：fakeorigin.py 缺失 → 返回 1；启动即退出（含 --verify 不符）→ FAIL 并返回 1
# ============================================================================
start_origin() {
    local size_mb="${1:-}" expect_sha="${2:-}"
    [[ -n "$size_mb" ]] || { echo "start_origin: 缺 size_mb" >&2; return 2; }

    if [[ ! -f "$ORIGIN" ]]; then
        echo "start_origin: 假源不存在：$ORIGIN" >&2
        return 1
    fi

    # 端口已被占（上一条残留）→ 直接报，别让后端起不来埋成谜之超时
    if __nf_port_open; then
        echo "start_origin: 127.0.0.1:$PORT 已被占用（残留进程？）" >&2
        return 1
    fi

    local -a args=(--size-mb "$size_mb" --port "$PORT")
    [[ -n "$expect_sha" ]] && args+=(--verify "$expect_sha")

    __nf_ensure_out
    local log="$OUT/logs/origin.log"
    python3 "$ORIGIN" "${args[@]}" >"$log" 2>&1 &
    ORIGIN_PID=$!

    local i
    for ((i = 0; i < 300; i++)); do           # 最多 30s
        if ! kill -0 "$ORIGIN_PID" 2>/dev/null; then
            local rc=0
            wait "$ORIGIN_PID" 2>/dev/null || rc=$?
            ORIGIN_PID=""
            echo "start_origin: 假源启动即退出 rc=$rc" >&2
            echo "---- $log ----" >&2
            sed -e 's/^/  /' "$log" >&2 2>/dev/null
            return 1
        fi
        if __nf_port_open; then
            return 0
        fi
        sleep 0.1
    done

    echo "start_origin: 等待 127.0.0.1:$PORT 就绪超时（30s）" >&2
    __nf_kill_origin
    return 1
}

__nf_port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; }

# 框架自己写日志/证据的目录由框架保证存在（OUT 可能在 load_env 之后被场景改掉）
__nf_ensure_out() {
    mkdir -p -- "$OUT/logs" 2>/dev/null && return 0
    echo "lib: 建不了 $OUT/logs" >&2
    return 1
}

# ============================================================================
# apply —— 幂等施加 netem
#   1) 先删旧 qdisc（不存在时内核报错属正常，这是唯一允许吞掉的一步）
#   2) MTU 改 1500（netem 无 mtu 参数，坑 1）
#   3) 挂 netem；内核报错原样透出
# ============================================================================
apply() {
    if [[ $# -eq 0 ]]; then
        echo "apply: 缺 netem 参数（例：apply loss 5%）" >&2
        return 2
    fi
    if [[ -z "${IFACE:-}" ]]; then
        echo "apply: IFACE 为空（没走 load_env？）" >&2
        return 2
    fi
    # 从这里开始就动接口了：先 arm，失败路径也要能被 trap 兜住
    __NF_ARMED=1

    "${SUDO[@]}" "$TC_BIN" qdisc del dev "$IFACE" root 2>/dev/null || true

    if ! "${SUDO[@]}" "$IP_BIN" link set dev "$IFACE" mtu "$NF_MTU_FAULT"; then
        echo "apply: ip link set dev $IFACE mtu $NF_MTU_FAULT 失败（内核报错见上）" >&2
        return 1
    fi

    if ! "${SUDO[@]}" "$TC_BIN" qdisc add dev "$IFACE" root netem "$@"; then
        echo "apply: tc qdisc add dev $IFACE root netem $* 失败（内核报错见上）" >&2
        # 事务性回滚：netem 没挂上，MTU 1500 就是纯污染（下一个场景会莫名在 1500 下跑）
        "${SUDO[@]}" "$IP_BIN" link set dev "$IFACE" mtu "$NF_MTU_CLEAN" ||
            echo "apply: 回滚 MTU=$NF_MTU_CLEAN 也失败，请手工 clear" >&2
        return 1
    fi

    # 判据落在产物上：确认 netem 真的在了，而不只是 tc 返回 0
    if ! "${SUDO[@]}" "$TC_BIN" qdisc show dev "$IFACE" 2>/dev/null | grep -q netem; then
        echo "apply: tc 返回成功但 $IFACE 上没有 netem（未生效）" >&2
        return 1
    fi
    return 0
}

# ============================================================================
# clear —— 幂等，可重复调用；trap EXIT 也会走这里
#   ① 删 netem ② 删 iptables 规则（完整 spec）③ MTU 还原 65536 ④ 杀假源
#   删完自检，自检不过 → 返回 1
#
#   ⚠ 只有「本 shell 动过手」（load_env / apply 置 __NF_ARMED=1）才会去碰接口。
#   否则环境里带着 IFACE=eth0 却没 load_env 时，这会去改桥接网卡的 MTU（IMPL-7）。
# ============================================================================
clear() {
    local rc=0

    if (( __NF_ARMED == 0 )); then
        echo "clear: 本 shell 未 load_env/apply（__NF_ARMED=0），不碰 $IFACE —— 无状态可清" >&2
        return 2
    fi

    "${SUDO[@]}" "$TC_BIN" qdisc del dev "$IFACE" root 2>/dev/null || true

    __nf_ipt_del_all || rc=1

    if ! "${SUDO[@]}" "$IP_BIN" link set dev "$IFACE" mtu "$NF_MTU_CLEAN"; then
        echo "clear: ip link set dev $IFACE mtu $NF_MTU_CLEAN 失败（内核报错见上）" >&2
        rc=1
    fi

    __nf_kill_origin
    __nf_self_check || rc=1
    return $rc
}

# 删掉本框架建过的 iptables 规则：必须带完整 spec（含 --reject-with tcp-reset），
# 否则内核答 "Bad rule" 且规则残留，之后所有场景莫名 RST。
__nf_ipt_del_all() {
    local -i n=0
    while ((n < 16)); do
        if "${SUDO[@]}" "$IPT_BIN" -D OUTPUT -p tcp --sport "$PORT" -j REJECT \
            --reject-with tcp-reset 2>"$__NF_ERRLOG"; then
            n+=1
            continue
        fi
        break
    done
    # 删失败可能是"本来就没有"，用残留情况判：真残留才把内核报错打出来
    if __nf_ipt_has_rule; then
        echo "clear: iptables 仍有残留规则（--sport $PORT）：$(cat "$__NF_ERRLOG")" >&2
        return 1
    fi
    return 0
}

__nf_ipt_has_rule() {
    "${SUDO[@]}" "$IPT_BIN" -S OUTPUT 2>/dev/null | grep -Eq -- "--sport $PORT( |\$)"
}

__nf_kill_origin() {
    [[ -n "${ORIGIN_PID:-}" ]] || return 0
    local i
    if __nf_pid_alive "$ORIGIN_PID"; then
        kill -TERM "$ORIGIN_PID" 2>/dev/null || true
        for ((i = 0; i < 20; i++)); do
            __nf_pid_alive "$ORIGIN_PID" || break
            sleep 0.1
        done
        { __nf_pid_alive "$ORIGIN_PID" && kill -KILL "$ORIGIN_PID" 2>/dev/null; } || true
    fi
    wait "$ORIGIN_PID" 2>/dev/null || true
    ORIGIN_PID=""
    return 0
}

# 从回收名单里摘掉已经 wait 过的 pid（不能对已回收的 pid 再 kill —— 会被复用）
__nf_untrack() {
    local pid="$1" x
    local -a keep=()
    for x in "${__NF_CHILDREN[@]}"; do
        [[ "$x" == "$pid" ]] || keep+=("$x")
    done
    __NF_CHILDREN=("${keep[@]}")
    return 0
}

# pid 是否「还活着」—— 僵尸不算：它已经干不了活，pid 也还没被回收，
# 对僵尸补 KILL 毫无意义，更糟的是 kill -0 对僵尸返回成功，
# 会让下面的等待循环白转满 1s（实测 20 次检查后才发 KILL）。
__nf_pid_alive() {
    local pid="$1" st
    kill -0 "$pid" 2>/dev/null || return 1
    # /proc/<pid>/stat 的 comm 字段可能带空格和括号，先切掉 " ) " 之前的部分
    st="$(sed -e 's/^.*) //' -e 's/ .*//' "/proc/$pid/stat" 2>/dev/null)"
    [[ -n "$st" && "$st" != "Z" ]]
}

# 被测进程若还在跑（外部 kill -TERM 场景脚本 / set -e 在 spawn 与 wait 之间退出），
# 必须收掉 —— 否则它会继续写 CASE_OUT，喂给下一条场景的残留通道。
__nf_reap_children() {
    local pid i
    for pid in "${__NF_CHILDREN[@]}"; do
        __nf_pid_alive "$pid" || continue
        kill -TERM "$pid" 2>/dev/null || true
        for ((i = 0; i < 20; i++)); do
            __nf_pid_alive "$pid" || break
            sleep 0.05
        done
        # 循环已经等到它消失就别再补刀：对已回收 pid 发信号是 pid 复用误杀的唯一入口
        { __nf_pid_alive "$pid" && kill -KILL "$pid" 2>/dev/null; } || true
        wait "$pid" 2>/dev/null || true
    done
    __NF_CHILDREN=()
    # 数组清空了，这两个独立变量也必须一起清：否则后续 __nf_stop_peer_sampler /
    # __nf_kill_origin 会拿着已回收的 pid 再发一次信号（REV-1）。
    CASE_PEERS_PID=""
    ORIGIN_PID=""
    return 0
}

# 取当前 MTU：必须按「整词 mtu」取第一个，不能用 grep 'mtu [0-9]*' ——
# 带 minmtu/maxmtu 的设备会同时匹配到 minmtu/maxmtu 的数字，得到 "1500 68 65535"，
# 于是恒不等于 65536 → self_check 永远报残留（IMPL-8）。
__nf_cur_mtu() {
    "$IP_BIN" -o link show dev "$IFACE" 2>/dev/null | head -1 |
        awk '{ for (i = 1; i <= NF; i++) if ($i == "mtu") { print $(i + 1); exit } }'
}

# 只读体检：netem 无残留 / 本框架的 iptables 规则无残留 / MTU 已还原
#   未 arm（没 load_env/apply）时没有属于本框架的状态可查，直接说清楚并放行。
__nf_self_check() {
    local rc=0 mtu

    if (( __NF_ARMED == 0 )); then
        echo "self_check: 本 shell 未 load_env/apply（__NF_ARMED=0），无状态可查" >&2
        return 0
    fi

    mtu="$(__nf_cur_mtu)"
    if [[ "$mtu" != "$NF_MTU_CLEAN" ]]; then
        echo "self_check: $IFACE MTU=$mtu，期望 $NF_MTU_CLEAN（clear 漏还原）" >&2
        rc=1
    fi

    if "${SUDO[@]}" "$TC_BIN" qdisc show dev "$IFACE" 2>/dev/null | grep -q netem; then
        echo "self_check: $IFACE 上还有 netem 残留" >&2
        "${SUDO[@]}" "$TC_BIN" qdisc show dev "$IFACE" >&2
        rc=1
    fi

    # 只判「本框架那条规则还在不在」，不判整条链的行数 ——
    # 装了 docker/ufw 的机器上整条链必然 >2 行，按行数判会全员误报。
    if __nf_ipt_has_rule; then
        echo "self_check: iptables 仍有本框架的残留规则：" >&2
        "${SUDO[@]}" "$IPT_BIN" -S OUTPUT 2>/dev/null |
            grep -E -- "--sport $PORT( |\$)" >&2
        rc=1
    fi
    return $rc
}

self_check() { __nf_self_check; }

# ============================================================================
# run_case <name> <断言函数> [参数...] [-- <断言函数> [参数...]]...
#
# 场景侧输入（全局，run_case 读）：
#   CASE_CMD        数组，$BIN 之后的命令行
#   CASE_OUT        期望落盘的文件（给才算吞吐、才做 sha1）
#                   ⚠ run_case 起跑前会 rm -f 它，并用 mtime >= 起跑时刻 判本轮新鲜度；
#                     mtime 更早 → `out=stale(...)` 直接 FAIL（否则上一轮成品会喂出假 PASS）
#   CASE_TIMEOUT_S  超时秒数，默认 120
#
# run_case 测完之后给断言函数的"实测值注入"（只对这几个内置断言生效，
# 因为它们的签名第一个参数就是实测值；自定义断言函数直接读下面的全局）：
#   assert_rate       注入 CASE_RATE_BPS
#   assert_sha        注入 CASE_OUT
#   assert_local_only 注入 CASE_PID（**但显式传了 pid 时不注入**，以参数为准）
#
# 可用全局：CASE_RC CASE_ELAPSED_MS CASE_BYTES CASE_RATE_BPS CASE_TIMEOUT
#           CASE_STALE CASE_OUT CASE_OUT_MTIME CASE_LOG CASE_PEERS CASE_PID
# 判定行恒定带 `rc=$CASE_RC`（T3.3 的「exit≠0」判据要用，断言之争不靠它）。
# 断言函数通过 return 0/non-0 表态，把证据串写进 ASSERT_DETAIL（例 rate=... sha=ok）；
# 由 run_case 统一打印唯一一行判定（PASS/FAIL）。断言成功但没写 detail → 记 `<fn>=ok`。
# ============================================================================
run_case() {
    local name="${1:-}"
    [[ -n "$name" ]] || { echo "run_case: 缺 name" >&2; return 2; }
    shift
    local -a aspec=("$@")

    if [[ -z "${BIN:-}" ]]; then
        __nf_verdict FAIL "$name" "bin=unset"
        return 1
    fi
    if ! command -v "$BIN" >/dev/null 2>&1 && [[ ! -x "$BIN" ]]; then
        __nf_verdict FAIL "$name" "bin=missing($BIN)"
        return 1
    fi

    local -a cmd=("$BIN")
    (( ${#CASE_CMD[@]} > 0 )) && cmd+=("${CASE_CMD[@]}")

    # spec §5.5：禁止有动作无断言 —— 没给断言就别跑，别产出无意义的 PASS
    if (( ${#aspec[@]} == 0 )); then
        __nf_verdict FAIL "$name" "assert=none(禁止有动作无断言)"
        return 1
    fi
    if ! __nf_ensure_out; then
        __nf_verdict FAIL "$name" "out=uncreatable($OUT/logs)"
        return 1
    fi

    CASE_LOG="$OUT/logs/$name.log"
    CASE_TIMEOUT=0
    CASE_STALE=0
    CASE_RC=0
    CASE_BYTES=0
    CASE_RATE_BPS=0
    ASSERT_DETAIL=""

    # 本轮产物必须从零开始：上一轮的成品若留在这里，被测程序 t≈0 就失败时
    # 后面会 stat 到它、assert_sha 读它，打出一个 sha=ok 的假 PASS。
    if [[ -n "$CASE_OUT" ]]; then
        rm -f -- "$CASE_OUT" 2>/dev/null ||
            echo "run_case: 删不掉上一轮产物 $CASE_OUT" >&2
    fi

    local t0 t1 t0_epoch
    t0="$(date +%s%N)"
    t0_epoch="$(date +%s)"

    "${cmd[@]}" >"$CASE_LOG" 2>&1 &
    CASE_PID=$!
    __NF_CHILDREN+=("$CASE_PID")

    __nf_start_peer_sampler "$CASE_PID"

    local -i limit_ms=$((CASE_TIMEOUT_S * 1000)) elapsed_ms=0
    while kill -0 "$CASE_PID" 2>/dev/null; do
        elapsed_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
        if ((elapsed_ms >= limit_ms)); then
            CASE_TIMEOUT=1
            kill -TERM "$CASE_PID" 2>/dev/null || true
            sleep 1
            kill -KILL "$CASE_PID" 2>/dev/null || true
            break
        fi
        sleep 0.05
    done
    CASE_RC=0
    wait "$CASE_PID" 2>/dev/null || CASE_RC=$?
    __nf_untrack "$CASE_PID"

    t1="$(date +%s%N)"
    CASE_ELAPSED_MS=$(( (t1 - t0) / 1000000 ))
    __nf_stop_peer_sampler

    # CASE_BYTES 的语义 = **本轮新生成**的字节数：mtime 早于起跑时刻的一律不认
    CASE_OUT_MTIME=""
    if [[ -n "$CASE_OUT" && -f "$CASE_OUT" ]]; then
        CASE_OUT_MTIME="$(stat -c %Y -- "$CASE_OUT" 2>/dev/null || echo 0)"
        if ((CASE_OUT_MTIME < t0_epoch)); then
            CASE_STALE=1
        else
            CASE_BYTES="$(stat -c %s -- "$CASE_OUT")"
        fi
    fi
    CASE_RATE_BPS=0
    if ((CASE_BYTES > 0 && CASE_ELAPSED_MS > 0)); then
        CASE_RATE_BPS="$(awk -v b="$CASE_BYTES" -v ms="$CASE_ELAPSED_MS" \
            'BEGIN{ printf "%d", b * 8000 / ms }')"
    fi

    local -i ok=0
    local detail="rc=$CASE_RC"       # rc 恒定在判定行里（T3.3 要靠它判 exit≠0）
    if ((CASE_STALE == 1)); then
        ok=1
        detail="$detail out=stale(mtime=$CASE_OUT_MTIME<t0=$t0_epoch)"
    fi
    if ((CASE_TIMEOUT == 1)); then
        ok=1
        detail="$detail timeout=${CASE_TIMEOUT_S}s"
    fi

    __nf_run_asserts "${aspec[@]}" || ok=1
    detail="${detail}${detail:+ }${ASSERT_DETAIL}"

    if ((ok == 0)); then
        __nf_verdict PASS "$name" "$detail"
        return 0
    fi
    __nf_verdict FAIL "$name" "$detail"
    return 1
}

# 断言组解析：遇到 `--` 切下一组
#   注入规则：只有「第一个参数就是实测值」的内置断言被注入；
#   显式传了参数的（assert_local_only）用调用方给的，不覆盖。
__nf_run_asserts() {
    local -i ok=0
    local out="" fn
    local -a grp inject ga
    while [[ $# -gt 0 ]]; do
        grp=()
        while [[ $# -gt 0 && "$1" != "--" ]]; do
            grp+=("$1")
            shift
        done
        [[ $# -gt 0 ]] && shift
        (( ${#grp[@]} == 0 )) && continue

        fn="${grp[0]}"
        ga=("${grp[@]:1}")
        inject=()
        case "$fn" in
            assert_rate)  inject=("$CASE_RATE_BPS") ;;
            assert_sha)   inject=("$CASE_OUT") ;;
            assert_local_only)
                (( ${#ga[@]} == 0 )) && inject=("$CASE_PID")
                ;;
        esac

        ASSERT_DETAIL=""
        local -i sub=0
        if ! declare -F "$fn" >/dev/null; then
            ASSERT_DETAIL="assert=$fn(undefined)"
            sub=1
        elif ! "$fn" "${inject[@]}" "${ga[@]}"; then
            sub=1
        fi
        (( sub != 0 )) && ok=1
        if [[ -z "$ASSERT_DETAIL" ]]; then
            # 断言成功也可能什么都没写：不能一律标 failed
            (( sub == 0 )) && ASSERT_DETAIL="$fn=ok" || ASSERT_DETAIL="$fn=failed"
        fi
        out="${out}${out:+ }$ASSERT_DETAIL"
    done
    ASSERT_DETAIL="$out"
    return $ok
}

__nf_verdict() {   # PASS|FAIL  name  detail
    local line="$1 $2"
    [[ -n "${3:-}" ]] && line="$line $3"
    printf '%s\n' "$line"
    VERDICTS+=("$line")
    [[ -n "${VERDICT_FILE:-}" ]] && printf '%s\n' "$line" >>"$VERDICT_FILE"
    return 0
}

# 运行期间采样被测进程的全部对端（assert_local_only 的证据来源；
# 进程一退出 socket 就没了，所以必须边跑边采）
# ss 的 stderr 单独落文件：采样为空时要能把原因（没装 iproute2 / 无权限）带进判定行
__nf_start_peer_sampler() {
    local pid="$1"
    local i
    local fast=0
    CASE_PEERS="$OUT/logs/peers.$pid.log"
    CASE_PEERS_ERR="$OUT/logs/peers.$pid.err"
    : >"$CASE_PEERS"
    : >"$CASE_PEERS_ERR"
    : >"$CASE_PEERS_ERR.tmp"
    (
        while kill -0 "$pid" 2>/dev/null; do
            ss -tnp 2>"$CASE_PEERS_ERR.tmp" |
                awk -v p="pid=$pid," 'index($0, p) { print $5 }'
            # ss 的报错只留首条（≤3 行）：不限的话 120s 能刷到 500+ 行，
            # 走 no_evidence 分支时会把整坨拼进一行判定（IMPL-9）。
            if [[ -s "$CASE_PEERS_ERR.tmp" && ! -s "$CASE_PEERS_ERR" ]]; then
                head -3 -- "$CASE_PEERS_ERR.tmp" >"$CASE_PEERS_ERR"
            fi
            # 自适应间隔：还没采到证据时 0.02s，采到即退到 0.2s。
            # 另设 50 轮上限（≈2.3s）：pid 匹配不上的病态场景（如 BIN=timeout
            # 让 CASE_PID 拿到 wrapper），证据永远为空，若不加限会全程 50Hz 跑 ss
            # —— 对 l2p_loss_20 那种时序敏感场景等于加了 56% 单核的恒定负载。
            if [[ -s "$CASE_PEERS" ]] || (( ++fast > 50 )); then
                sleep 0.2
            else
                sleep 0.02
            fi
        done
    ) >>"$CASE_PEERS" 2>/dev/null &
    CASE_PEERS_PID=$!
    __NF_CHILDREN+=("$CASE_PEERS_PID")
    for ((i = 0; i < 20; i++)); do
        [[ -s "$CASE_PEERS" ]] && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.05
    done
    return 0
}

# ss 报错摘要：最多 2 行、换行并成 `;`、括号统一成方括号以免打断 detail 的括号配对
__nf_ss_err_detail() {
    local f="${1:-${CASE_PEERS_ERR:-}}"
    [[ -n "$f" && -s "$f" ]] || return 0
    head -2 -- "$f" 2>/dev/null | tr '\n' ';' | tr '()' '[]' | sed -e 's/;$//'
}

__nf_stop_peer_sampler() {
    [[ -n "${CASE_PEERS_PID:-}" ]] || return 0
    # 先判活（僵尸不算活）再发信号：reap 收掉它之后，这个 pid 可能已经被别人复用。
    if __nf_pid_alive "$CASE_PEERS_PID"; then
        kill -TERM "$CASE_PEERS_PID" 2>/dev/null || true
    fi
    wait "$CASE_PEERS_PID" 2>/dev/null || true
    __nf_untrack "$CASE_PEERS_PID"
    CASE_PEERS_PID=""
    return 0
}

# ============================================================================
# assert_rate <实测bps> <目标bps> [容差=0.2]
# ============================================================================
assert_rate() {
    local actual="${1-}" target="${2-}" tol="${3:-0.2}"
    if [[ -z "$actual" || -z "$target" ]]; then
        ASSERT_DETAIL="rate=unset(actual=$actual expect=$target)"
        return 2
    fi

    local lo hi dev ai ti
    ai="$(awk -v v="$actual" 'BEGIN{ printf "%d", v + 0 }')"
    ti="$(awk -v v="$target" 'BEGIN{ printf "%d", v + 0 }')"
    lo="$(awk -v t="$target" -v k="$tol" 'BEGIN{ printf "%d", t * (1 - k) }')"
    hi="$(awk -v t="$target" -v k="$tol" 'BEGIN{ printf "%d", t * (1 + k) }')"
    dev="$(awk -v a="$actual" -v t="$target" \
        'BEGIN{ if (t + 0 == 0) { print "n/a"; exit } printf "%+.3f", (a - t) / t }')"

    local -i pass=0
    awk -v a="$actual" -v t="$target" -v k="$tol" \
        'BEGIN{ exit !(t + 0 > 0 && a + 0 >= t * (1 - k) && a + 0 <= t * (1 + k)) }' && pass=1

    if ((pass == 1)); then
        ASSERT_DETAIL="rate=$ai expect=$ti tol=$tol"
        return 0
    fi
    ASSERT_DETAIL="rate=$ai expect=$ti tol=$tol range=$lo~$hi dev=$dev"
    return 1
}

# ============================================================================
# assert_sha <文件> <期望sha1>
# ============================================================================
assert_sha() {
    local f="${1-}" expect="${2-}"
    if [[ -z "$f" || -z "$expect" ]]; then
        ASSERT_DETAIL="sha=unset(file=$f expect=$expect)"
        return 2
    fi
    if [[ ! -f "$f" ]]; then
        ASSERT_DETAIL="sha=absent(file=$f)"
        return 1
    fi
    local got
    got="$(sha1sum -- "$f" 2>/dev/null | cut -d' ' -f1)"
    if [[ "$got" == "$expect" ]]; then
        ASSERT_DETAIL="sha=ok"
        return 0
    fi
    ASSERT_DETAIL="sha=mismatch(got=$got,expect=$expect,bytes=$(stat -c %s -- "$f"))"
    return 1
}

# ============================================================================
# assert_local_only [pid] —— 对端必须全是 127.0.0.1（spec §4.3 禁公网）
#
#   证据归属：采样文件 CASE_PEERS 只属于 CASE_PID。
#   - pid == CASE_PID（或没显式传参）→ 用采样证据
#   - 显式问别的 pid → 一律现场 ss，**不许借用 CASE_PID 的证据**
#     （否则「问一个不存在的 pid」会因为文件里有别人 的本地对端而判 ok，REV-2）
#   进程已退出且没有归属于它的证据 → **没有证据就不是通过**：no_evidence FAIL。
# ============================================================================
assert_local_only() {
    local pid="${1-}"
    if [[ -z "$pid" ]]; then
        ASSERT_DETAIL="local_only=unset(pid)"
        return 2
    fi

    local -a peers=() real=()
    local src="" err="" p

    if [[ -n "${CASE_PID:-}" && "$pid" == "${CASE_PID:-}" &&
        -n "${CASE_PEERS:-}" && -s "${CASE_PEERS:-}" ]]; then
        src="sampled"
        mapfile -t peers < <(sort -u -- "$CASE_PEERS")
        err="$(__nf_ss_err_detail)"
    elif kill -0 "$pid" 2>/dev/null; then
        # 兜底只在进程还活着时有意义；ss 的 stderr 必须能被看到
        src="live"
        local errf="${OUT:-/tmp}/logs/ss.live.err"
        mapfile -t peers < <(
            ss -tnp 2>"$errf" | awk -v q="pid=$pid," 'index($0, q) { print $5 }' | sort -u
        )
        err="$(__nf_ss_err_detail "$errf")"
    else
        # 这条是最需要诊断的路径：没证据时把 ss 的报错也带上
        err="$(__nf_ss_err_detail)"
        ASSERT_DETAIL="local_only=no_evidence(peers=0,pid=$pid 已退出且无采样证据${err:+,ss_err=$err})"
        return 1
    fi

    for p in "${peers[@]}"; do [[ -n "$p" ]] && real+=("$p"); done

    if (( ${#real[@]} == 0 )); then
        ASSERT_DETAIL="local_only=no_evidence(peers=0,src=$src${err:+,ss_err=$err})"
        return 1
    fi

    local -i bad=0
    local first_bad=""
    for p in "${real[@]}"; do
        case "$p" in
            127.* | \[::1\]:* | ::ffff:127.*) ;;
            *) bad+=1; [[ -z "$first_bad" ]] && first_bad="$p" ;;
        esac
    done

    if ((bad == 0)); then
        ASSERT_DETAIL="local_only=ok(peers=${#real[@]},src=$src)"
        return 0
    fi
    ASSERT_DETAIL="local_only=bad(peers=${#real[@]},bad=$bad,first_bad=$first_bad,src=$src)"
    return 1
}

# ============================================================================
# assert_rc <期望rc|nonzero> —— 读 CASE_RC（T3.3 的「exit≠0」判据靠它）
#   实测 rc 已由 run_case 恒定写进判定行（`rc=N`），这里只补 `expect=`，不重复报 rc。
# ============================================================================
assert_rc() {
    local expect="${1-}"
    if [[ -z "$expect" ]]; then
        ASSERT_DETAIL="expect=unset(需要 <期望rc|nonzero>)"
        return 2
    fi
    if [[ "$expect" == "nonzero" ]]; then
        ASSERT_DETAIL="expect=nonzero"
        [[ "${CASE_RC:-0}" != "0" ]] && return 0
        return 1
    fi
    ASSERT_DETAIL="expect=$expect"
    [[ "${CASE_RC:-0}" == "$expect" ]] && return 0
    return 1
}

# ============================================================================
# emit_report <输出路径> —— 把本次判定行写成 markdown 表
#   本进程有 VERDICTS 就用它（避免读到别的场景的旧行）；否则退回读 VERDICT_FILE
# ============================================================================
emit_report() {
    local path="${1:-}"
    [[ -n "$path" ]] || { echo "emit_report: 缺输出路径" >&2; return 2; }

    local -a lines=()
    if (( ${#VERDICTS[@]} > 0 )); then
        lines=("${VERDICTS[@]}")
    elif [[ -n "${VERDICT_FILE:-}" && -s "$VERDICT_FILE" ]]; then
        mapfile -t lines <"$VERDICT_FILE"
    fi

    local -i pass=0 fail=0 i=0
    local l verdict rest name detail
    {
        echo "# net_faults 判定汇总"
        echo
        echo "| # | 判定 | 场景 | 证据 |"
        echo "|---|---|---|---|"
        for l in "${lines[@]}"; do
            [[ -z "$l" ]] && continue
            i+=1
            verdict="${l%% *}"
            rest="${l#* }"
            name="${rest%% *}"
            detail=""
            [[ "$rest" == *" "* ]] && detail="${rest#* }"
            detail="${detail//|/\\|}"
            [[ "$verdict" == "PASS" ]] && pass+=1 || fail+=1
            printf '| %d | %s | %s | %s |\n' "$i" "$verdict" "$name" "$detail"
        done
        echo
        printf '合计：%d 条，PASS %d，FAIL %d\n' "$i" "$pass" "$fail"
    } >"$path" || { echo "emit_report: 写不了 $path" >&2; return 1; }
    return 0
}

# ============================================================================
# EXIT 兜底清理（tasks.md T0.1：场景中途失败也要清 netem）
#   场景脚本设置 NETFAULTS_NO_TRAP=1 可关闭（由调用方自己负责 clear）
#
#   原则：**静默删除 + 响亮自检**（审查 P1-4）。
#   删除动作的噪声可以吞；但自检不过必须打到 stderr 并落标记文件 ——
#   否则残留会被记到下一条场景头上（误归因），而本条的 PASS 一点不受影响。
# ============================================================================
if [[ -z ${NETFAULTS_NO_TRAP:-} && $- != *i* ]]; then
    trap '__nf_on_exit' EXIT
fi

__nf_on_exit() {
    local rc=$?

    # 只 source 了 lib 却没 load_env / apply 的 shell（__NF_ARMED=0）没有可清的东西：
    # 放行，否则会对一个没动过的设备做 tc/ip 调用（环境里带 IFACE=eth0 时会改桥接网卡）。
    if (( __NF_ARMED == 0 )); then
        rm -f -- "${__NF_ERRLOG:-}" 2>/dev/null
        return $rc
    fi

    # ① 先收被测进程：它会继续写 CASE_OUT，是下一条场景"残留成品"的来源
    __nf_reap_children
    __nf_stop_peer_sampler
    __nf_kill_origin

    # ② 静默删除
    "${SUDO[@]}" "$TC_BIN" qdisc del dev "$IFACE" root >/dev/null 2>&1 || true
    __nf_ipt_del_all >/dev/null 2>&1 || true
    "${SUDO[@]}" "$IP_BIN" link set dev "$IFACE" mtu "$NF_MTU_CLEAN" >/dev/null 2>&1 || true

    # ③ 响亮自检：不过就出声 + 留痕，不改本次退出码（本次结论已由显式 clear 决定）
    if ! __nf_self_check >/dev/null 2>&1; then
        local marker="${OUT:-/tmp}/logs/trap_self_check.fail"
        mkdir -p -- "$(dirname -- "$marker")" 2>/dev/null || true
        {
            printf 'trap 兜底清理后自检未过 %s iface=%s\n' "$(date -Is)" "$IFACE"
            __nf_self_check 2>&1 | sed -e 's/^/  /'
        } >>"$marker" 2>/dev/null || true
        echo "trap: 兜底清理后自检未过（残留会污染下一条场景），已记入 $marker" >&2
    fi

    rm -f -- "${__NF_ERRLOG:-}" 2>/dev/null
    return $rc
}
