#!/bin/bash
# test-wifi-reconnect.sh - WiFi 自动重连：手动验证清单
#
# 这个脚本**不是**自动化测试。断线是物理事件（关掉路由器 / 拔掉 AP），主机无法
# 代劳，所以除「AT+CWSTATE 能否回包」外，其余各项都要人看着串口日志判断。
#
# 它因此只做两件事：发一条 AT 命令并核对回包，然后把「该看到什么」印成清单。
# 旧版本不论发生什么都会打印「✅ WiFi自动重连功能已实现」，并给出几条与固件
# 实际输出不符的日志样例（tag 写成 `[wifi-mgr]`，真实的是 `[wifi-sup]`），
# 还标了一个凭空而来的内存占用数字——这些都已删除。
#
# 可自动化的上板验在 tools/s3_full_test.py。
#
# 用法: ./tools/test-wifi-reconnect.sh [PORT] [DEVICE_IP]

set -uo pipefail

PORT=${1:-"/dev/cu.usbserial-*"}
DEVICE_IP=${2:-"192.168.1.100"}

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo "测试配置:"
echo "  串口: ${PORT}"
echo "  设备IP: ${DEVICE_IP}"
echo ""

ACTUAL_PORT=$(ls $PORT 2>/dev/null | head -1)
if [ -z "$ACTUAL_PORT" ]; then
    echo -e "${RED}❌ 错误: 未找到串口设备${NC}"
    echo "请连接ESP32设备并确认串口路径"
    exit 1
fi

echo -e "${GREEN}✓ 找到串口: ${ACTUAL_PORT}${NC}"
echo ""

# ---------------------------------------------------------------- 自动部分 --
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${BLUE}测试 1: 基本WiFi状态查询${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

stty -f "$ACTUAL_PORT" 115200 raw -echo 2>/dev/null
exec 3<>"$ACTUAL_PORT" || { echo -e "${RED}无法打开 ${ACTUAL_PORT}${NC}"; exit 1; }
printf 'AT+CWSTATE\r\n' >&3
# 读 3 秒：`cat` 阻塞到被 kill，收到的字节落到临时文件（后台任务没法用
# 命令替换捕获，所以不能写 `BUF=$(cat ...)`）。
_tmp=$(mktemp)
cat <&3 >"$_tmp" 2>/dev/null &
_reader=$!
sleep 3
kill "$_reader" 2>/dev/null
wait "$_reader" 2>/dev/null
exec 3>&-
BUF=$(cat "$_tmp" 2>/dev/null)
rm -f "$_tmp"

echo "回包:"
printf '%s\n' "$BUF" | tr -d '\r' | sed 's/^/  /'
echo ""

if printf '%s' "$BUF" | tr -d '\r' | grep -qE '\+CWSTATE:'; then
    echo -e "${GREEN}✓ PASS${NC}: AT+CWSTATE 返回 +CWSTATE:"
    rc=0
else
    echo -e "${RED}✗ FAIL${NC}: 未收到 +CWSTATE: 回包"
    echo "  0 = 未连接, 4 = 已连接但无 IP（见 docs/AT_COMMAND_REFERENCE.md）"
    rc=1
fi
echo ""

# ---------------------------------------------------------------- 手动部分 --
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${BLUE}测试 2-5: 需要人工判断（观察串口日志）${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo -e "${YELLOW}以下每一项都由「你做的动作 + 串口日志」共同判定，脚本无法替你断言。${NC}"
echo ""

echo "【测试 2】HTTPS 可达性与设备状态"
if [ -n "$(curl -s -f "http://$DEVICE_IP/api/status" 2>/dev/null)" ]; then
    echo -e "  ${GREEN}✓${NC} 设备 HTTP 服务可达：http://${DEVICE_IP}/api/status 有响应"
else
    echo -e "  ${YELLOW}⚠${NC} 无法访问 http://${DEVICE_IP}/api/status（未连 WiFi，或 IP 参数不对）"
fi
echo ""

echo "【测试 3】断线恢复 —— 手动步骤："
echo "  1. 确认设备已连接 WiFi"
echo "  2. 关闭路由器，或禁用该设备的连接"
echo "  3. 串口应出现（这些是固件真实打印的字符串，见"
echo "     main.rs / wifi_manager.rs）："
echo "       [wifi-sup] LINK DOWN (consecutive=1) ip=… rssi=… dBm - reconnecting"
echo "       [wifi-mgr] ⚠️ 检测到断线，准备重连..."
echo "       [wifi-mgr] 🔄 重连尝试 #1, 下次退避: 2s"
echo "  4. 重新开启路由器"
echo "  5. 串口应出现："
echo "       [wifi-mgr] ✅ 重连成功！"
echo "       [wifi-sup] LINK UP ip=… rssi=… dBm"
echo "  判定：第 3、5 步的现象都出现才算通过。只出现一半＝失败。"
echo ""

echo "【测试 4】信号质量告警"
echo "  固件**只在信号差时**打印，正常信号不打印。把设备逐步远离 AP："
echo "       [wifi-sup] ⚠️ 信号质量差: Poor (RSSI: -75 dBm)"
echo "  判定：RSSI 恶化到 Poor/VeryPoor 时出现该行。阈值见"
echo "        wifi_manager.rs 的 ConnectionQuality（Poor < -70，VeryPoor <= -80）。"
echo "  注意：RSSI 读取失败时评级是 Unknown，**不会**打印告警，也不会误报成"
echo "        Excellent——这是刻意区分，见同文件的注释。"
echo ""

echo "【测试 5】连接统计"
echo "  心跳行里带 `state=…, retries=…, connects=…, disconnects=…`："
echo "       [wifi-sup] heartbeat ip=… rssi=… dBm [state=Connected, retries=0, connects=1, disconnects=0]"
echo "  判定：断线一次后 disconnects 加一、重连成功后 connects 加一、retries 归零。"
echo ""

echo "监听串口日志（按 Ctrl+C 退出）："
if command -v screen >/dev/null 2>&1; then
    echo "  screen ${ACTUAL_PORT} 115200"
elif command -v minicom >/dev/null 2>&1; then
    echo "  minicom -D ${ACTUAL_PORT} -b 115200"
else
    echo "  未找到 screen/minicom：brew install screen"
    echo "  或直接：cat ${ACTUAL_PORT}"
fi
echo ""

exit $rc
