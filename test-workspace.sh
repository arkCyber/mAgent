#!/bin/bash
# test-workspace.sh - 主机侧（非固件）检查：fmt + clippy + test
#
# 只用**默认成员**，不用 `--workspace`：固件 crate 的
# `#[link_section = ".vector_table.interrupts"]` 在 macOS 上生成非法 Mach-O
# 段名，`--workspace` 会直接死在 `nrf52840-pac`（见 docs/BACKLOG.md）。
# 固件用 ./firmware/esp32-app/build-c61.sh / build-s3.sh 单独构建。
#
# 用法: ./test-workspace.sh
set -uo pipefail

echo "=========================================="
echo "  mAgent 主机侧检查"
echo "=========================================="
echo ""

fail=0

# 格式**仅供提示，不计入失败**：仓库当前有数百处既有的 rustfmt 漂移
# （`git stash -u` 后仍能数出 690+ 处 diff），CI 也因此把这一步设成
# `continue-on-error: true`（见 .github/workflows/ci.yml）。若在此处 fail=1，
# 这个脚本永远不可能变绿，作为门禁就没有意义了。归一化是一次独立的、
# 全仓库的改动，不在本检查的职责范围内。
echo "1) 代码格式 (cargo fmt --check，仅供参考)"
if cargo fmt --all -- --check >/dev/null 2>&1; then
    echo "   ✓ 格式正确"
else
    echo "   ⚠ 存在格式漂移（仓库既有状态，CI 同样不阻断；归一化请单独提交）"
fi
echo ""

echo "2) Clippy（默认成员，全部 target，警告即错误）"
if cargo clippy --all-targets -- -D warnings; then
    echo "   ✓ clippy 干净"
else
    echo "   ✗ clippy 有问题"
    fail=1
fi
echo ""

echo "3) 测试套件（默认成员：lib + 集成 + doc）"
if cargo test; then
    echo "   ✓ 测试通过"
else
    echo "   ✗ 测试失败"
    fail=1
fi
echo ""

# web3 / blockchain 是 default-off 的构建期 feature，第 2 步的默认 clippy
# 根本不会编译它们的代码。历史上正是这里藏过 dead import 和一条 clippy lint
# （见 CHANGELOG 2026-09-22）。cli 是唯一带这些 feature 的 crate。
echo "4) Clippy（cli，--features web3,blockchain，警告即错误）"
if cargo clippy -p magent --features web3,blockchain --all-targets -- -D warnings; then
    echo "   ✓ 特性门控代码 clippy 干净"
else
    echo "   ✗ 特性门控代码 clippy 有问题"
    fail=1
fi
echo ""

echo "5) 测试（cli，--features web3,blockchain）"
if cargo test -p magent --features web3,blockchain --lib; then
    echo "   ✓ 特性门控测试通过"
else
    echo "   ✗ 特性门控测试失败"
    fail=1
fi
echo ""

# 每个 feature 单独编译一遍，**必须带 --all-targets**：`#[cfg(test)]` 模块只有
# 在测试目标里才会被编译，而历史上正是那里藏过一个编译错误
# （agent_tools.rs 用了 `std::vec::Vec`，但 web3 不隐含 std，固件正是
# `default-features = false, features = ["web3"]` 的配置，见 CHANGELOG 2026-09-22）。
#
# `nrf52` 在 macOS 上必然失败（nrf52840-pac 的
# `#[link_section = ".vector_table.interrupts"]` 不是合法 Mach-O 段名），
# 属于已知平台限制，这里显式跳过而不是当作失败。
echo "6) 特性矩阵编译检查（magent-core，每个 feature 单独，--all-targets）"
feat_fail=0
for feat in monitoring security power wallet boot-key ingress web3_app \
            blockchain verifiable_credentials link_adapters mqtt manual \
            s3-8mb-psram web3_std web3 std; do
    if cargo check -p magent-core --no-default-features --features "$feat" --all-targets >/dev/null 2>&1; then
        echo "   ✓ $feat"
    else
        echo "   ✗ $feat"
        feat_fail=1
        fail=1
    fi
done
# nrf52：仅确认失败原因是平台限制，而不是新错误。
#
# 必须先把输出接住再 grep，不能写成 `cargo check ... | grep -q`：脚本开了
# `set -o pipefail`，而 cargo 在这里**必然**失败，于是整条管道的退出码是
# cargo 的 101 而不是 grep 的 0，`if` 永远走 else 分支（明明匹配到了却报
# 「原因不符」）。把两个命令拆开，管道就只剩 printf（总是成功）+ grep。
nrf_out=$(cargo check -p magent-core --no-default-features --features nrf52 --all-targets 2>&1)
if printf '%s' "$nrf_out" | grep -q "invalid Mach-O section specifier"; then
    echo "   ⊘ nrf52（跳过：macOS 非法 Mach-O 段名，已知平台限制）"
else
    echo "   ✗ nrf52 失败原因与已知的 Mach-O 限制不符，请检查"
    feat_fail=1
    fail=1
fi
[ "$feat_fail" -eq 0 ] && echo "   ✓ 特性矩阵全部通过"
echo ""

echo "=========================================="
if [ "$fail" -eq 0 ]; then
    echo "✅ 全部通过"
    exit 0
else
    echo "❌ 有检查未通过（见上方 ✗）"
    exit 1
fi
