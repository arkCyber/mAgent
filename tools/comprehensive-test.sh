#!/bin/bash
# comprehensive-test.sh - 设备冒烟测试（串口 + HTTP）
#
# 断言原则：每一条串口用例都必须**读到**固件的回应才判 PASS。旧版本只用
# `printf ... > $PORT` 写串口、从不读取，因此 `test_result $?` 只证明了
# 主机能打开 tty，跟固件是否响应毫无关系——串口部分永远是绿的。
#
# 完整的上板验收在 tools/s3_full_test.py（33 项，含 boot/AT/agent/内存/网络）；
# 本脚本是它的轻量补充，只做「板子活着吗」级别的检查。
#
# 用法: ./tools/comprehensive-test.sh [PORT] [DEVICE_IP]

# 不用 `set -e`：`((pass_count++))` 在值为 0 时返回 1，会让脚本在第一条
# PASS 处静默退出（旧版本正是如此）。
set -uo pipefail

PORT=${1:-"/dev/cu.usbserial-*"}
DEVICE_IP=${2:-"192.168.1.100"}

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║         mAgent 设备冒烟测试                                  ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""
echo "配置:"
echo "  串口: $PORT"
echo "  设备IP: $DEVICE_IP"
echo ""

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

test_count=0
pass_count=0
fail_count=0

test_result() {
    local rc=$1
    local name=$2
    if [ "$rc" -eq 0 ]; then
        echo -e "${GREEN}✓ PASS${NC}: $name"
        pass_count=$((pass_count + 1))
    else
        echo -e "${RED}✗ FAIL${NC}: $name"
        fail_count=$((fail_count + 1))
    fi
    test_count=$((test_count + 1))
}

# ---------------------------------------------------------------- serial ----
# 找一个存在的串口。`ls $PORT` 里的 glob 由 shell 展开（刻意不加引号）。
ACTUAL_PORT=$(ls $PORT 2>/dev/null | head -1)
SKIP_SERIAL=0
if [ -z "$ACTUAL_PORT" ]; then
    echo -e "${YELLOW}⚠ 警告: 未找到串口设备（${PORT}），跳过串口测试${NC}"
    SKIP_SERIAL=1
else
    echo "使用串口: $ACTUAL_PORT"
fi

# 发一条 AT 命令，读回串口输出，只有匹配 $pattern 才算通过。
#   send_and_expect <line> <extended-regex> [wait_seconds]
#
# fd 3 以读写方式打开 tty，一次 open 既发又收；`cat` 一直阻塞到超时被杀，
# 因此不会漏掉固件的回包。
send_and_expect() {
    local line="$1" pattern="$2" wait_s="${3:-3}"
    local buf

    # shellcheck disable=SC2064
    stty -f "$ACTUAL_PORT" 115200 raw -echo 2>/dev/null

    exec 3<>"$ACTUAL_PORT" || return 1
    printf '%s\r\n' "$line" >&3 || { exec 3>&-; return 1; }
    buf=$(_timeout "$wait_s" cat <&3 2>/dev/null)
    exec 3>&-

    [ -n "$buf" ] || { echo "    (no bytes read from $ACTUAL_PORT)"; return 1; }
    printf '%s' "$buf" | tr -d '\r' | grep -qE "$pattern" || {
        echo "    期望 /$pattern/，实收: $(printf '%s' "$buf" | tr -d '\r' | tail -3 | tr '\n' '|')"
        return 1
    }
    return 0
}

# macOS 没有自带 GNU `timeout`（coreutils 提供的是 `gtimeout`）；两者都缺时
# 退化到「后台跑 + 到点 kill」。
if command -v timeout >/dev/null 2>&1; then
    _timeout() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then
    _timeout() { gtimeout "$@"; }
else
    _timeout() {
        local secs="$1"; shift
        "$@" & local pid=$!
        ( sleep "$secs"; kill "$pid" 2>/dev/null ) &
        local wd=$!
        wait "$pid" 2>/dev/null
        kill "$wd" 2>/dev/null
    }
fi

if [ "$SKIP_SERIAL" -eq 0 ]; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "📡 测试 1: AT命令接口"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "测试: AT+GMR (版本信息)"
    send_and_expect 'AT+GMR' '\+GMR:|AT\+GMR' 3
    test_result $? "AT+GMR 返回版本信息"

    echo ""
    echo "测试: AT+CWSTATE (WiFi 状态)"
    send_and_expect 'AT+CWSTATE' '\+CWSTATE:' 3
    test_result $? "AT+CWSTATE 返回状态"

    echo ""
    echo "测试: 未知命令返回 +CMDER 错误码"
    send_and_expect 'AT+NOSUCHCMD' '\+CMDER:[0-9]+' 3
    test_result $? "未知命令返回 +CMDER"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🌐 测试 2: HTTP管理面板"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

echo "测试: GET /"
# 不带 --max-time 时，curl 会一直等到内核的 TCP 超时（几十秒到数分钟），脚本
# 看起来像卡死。设备不在线是常态，5 秒足够判定，超时即 FAIL。
if curl -s -f --connect-timeout 3 --max-time 5 "http://$DEVICE_IP/" -o /dev/null -w "%{http_code}" 2>/dev/null | grep -q "200"; then
    test_result 0 "HTTP根路径访问"
else
    test_result 1 "HTTP根路径访问 (设备可能未连接WiFi)"
fi

echo ""
echo "测试: GET /api/status"
STATUS=$(curl -s --connect-timeout 3 --max-time 5 "http://$DEVICE_IP/api/status" 2>/dev/null)
# 必须先确认非空：`jq .` 在空输入上返回 0，只看退出码会让「连不上」变成 PASS。
if [ -n "$STATUS" ] && printf '%s' "$STATUS" | jq . >/dev/null 2>&1; then
    test_result 0 "状态API返回有效JSON"
    echo "状态数据:"
    printf '%s' "$STATUS" | jq .
else
    test_result 1 "状态API (设备可能未连接WiFi)"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 测试结果汇总"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "总测试数: $test_count"
echo -e "${GREEN}通过: $pass_count${NC}"
if [ "$fail_count" -gt 0 ]; then
    echo -e "${RED}失败: $fail_count${NC}"
else
    echo "失败: 0"
fi
echo ""

if [ "$fail_count" -eq 0 ] && [ "$test_count" -gt 0 ]; then
    echo -e "${GREEN}✅ 所有测试通过！${NC}"
    exit 0
elif [ "$fail_count" -gt 0 ]; then
    echo -e "${YELLOW}⚠️  部分测试失败，请检查设备连接和配置${NC}"
    exit 1
else
    echo -e "${YELLOW}⚠️  未执行测试，请检查串口连接${NC}"
    exit 2
fi
