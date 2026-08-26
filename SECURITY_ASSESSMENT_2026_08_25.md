# MicroAgent 独立安全审计与加固报告

**日期**: 2026-08-25
**范围**: magent-core（Rust, no_std 优先）、ESP32-C61 / nRF52 固件、CLI、主机端 MCP 服务
**方法**: 以代码事实为准的独立审查（不采信既有审计文档的"✅ PASSED"断言，逐文件核对）
**验证**: `cargo test -p magent-core --features std,web3,wallet,verifiable_credentials` → **lib 590 passed / 0 failed**

> 说明：本报告中的发现均来自对当前工作区代码的逐行核对。凡标注"已验证"的项都给出了具体文件:行号证据。尚未能在本机交叉编译 ESP32 / nRF52 固件（macOS 无工具链），固件相关结论以静态审查为准。

---

## 1. TL;DR — 交付清单

| 项 | 状态 |
|---|---|
| 独立安全审查（全量） | ✅ 完成，见 §3 |
| 已加固项确认（凭据加密 / 常数时间 / 校验） | ✅ 见 §2 |
| 新增通用秘密封印原语 `seal_secret` / `open_secret` | ✅ 已实现 + 6 个回归测试通过 |
| 遗留风险 | 🟠 1 项中-高（LLM key 明文存 NVS），其余 Low/Info |
| 固件 LLM-key 封印接线 | 📋 给出精确改动方案（§4），因无法交叉编译未直接改 |

---

## 2. 已验证的既有加固（代码事实）

### 2.1 钱包私钥存储 — `web3/wallet/keystore.rs`
- **Argon2id**（`DEFAULT_MEMORY_KIB=1024`, time=2, p=1）口令派生 + **AES-256-GCM** 认证加密。
- 每次加密使用**新鲜 salt + nonce**，两次加密同一密钥的密文不同（防离线相等性攻击）。
- 篡改密文/salt → GCM 标签校验失败 → `InvalidPassphrase`（认证失败，而非返回损坏密钥）。

### 2.2 NVS 钱包索引抗损坏 — `web3/wallet/esp32_nvs.rs`
- `load_index` 区分三种状态：`Ok(Some)` / `Ok(None)`（首启正常）/ `Err(CorruptedIndex)`。
- 索引损坏（NVS 磨损/半写）**不会**被 `store_wallet` 静默覆盖为空白索引（避免毁掉全部钱包）。

### 2.3 Wi-Fi 密码封印（DBO2）— `wifi_pass_seal_v2.rs`
- HKDF-SHA256 派生**每条目独立** cipher key + mac key（info 域分离）。
- 完整性：`HMAC-SHA256(nonce||cipher)` 截断 16B，**常数时间比较**（`diff |= computed[i]^stored_mac[i]`）。
- 兼容迁移：DBO1 封印值、无前缀明文均能透明打开，写入时升级为 DBO2。

### 2.4 设备绑定密钥（BTDK1）— `boot_key.rs` / `device_key.rs`
- `dev_identity` 不再明文存储，用 eFuse + chip revision 经 Keccak256 派生 BTDK1 封印。
- 只 dump NVS 的攻击者拿不到设备身份与派生封印密钥。

### 2.5 CSPRNG UUID — `web3/verifiable_credentials.rs`
- 原固定种子 LCG + `static mut`（可预测、UB）已替换为 `getrandom` 真随机 + RFC 4122 v4 位。
- RNG 失败降级为 Nil UUID 并记录，不 panic。

### 2.6 私钥擦除 — `web3/identity.rs`
- `SecretKey::Drop` 用 `write_volatile` 逐字节清零 + `compiler_fence(SeqCst)` 固定顺序。

### 2.7 secp256k1 群阶校验（H10）— `web3/blockchain/secp256k1.rs`
- `from_bytes` 经 `SecretKey::from_slice` 拒绝 0 和 ≥ 群阶 n 的标量。
- `public_key()` / `inner()` 对越界密钥**传播错误而非 panic**（原 `.expect` 已去除）。

### 2.8 AT 命令输入校验面 — `at.rs` / `at_validate.rs`
- 无 panic / 无 alloc / 有界执行；SSID≤32、pass≤64、hostname≤32、model≤64、key≤128、URL≤512。
- 拒绝 NUL / 控制字节 / 非 UTF-8，主机端数百条恶意输入测试覆盖。

### 2.9 闪存错误传播（H1）— `storage.rs`
- `get_stats` 等路径已由 `if let Err(_){break;}` 改为 `?` 传播，读失败不再静默丢条目。

### 2.10 命令执行安全 — `cli/src/scheduler.rs`
- 使用 `Command::new(exe).arg(...)` 逐参传递，**非 shell**，无命令注入面。

### 2.11 启动健壮性 — `firmware/esp32-app/src/main.rs`
- TRNG 连续失败不再 `panic!`，进入 DEGRADED 模式（无签名），避免看门狗重启→NVS 磨损→变砖。

---

## 3. 发现的剩余风险（按严重度）

### 🟠 中-高 — LLM API key 以明文写入 NVS
- **位置**: `firmware/esp32-app/src/at_dispatch.rs:1075`（`AT+LLMCFG=` Set）、`main.rs:985`（`provision_llm_config` 启动写入）。
- **影响**: DeepSeek API key 是云端计费凭据；攻击者 dump NVS（无需读 eFuse）即得明文 key。而 Wi-Fi 密码已封印，此处为**凭据存储不一致**。
- **缓解**: `AT+LLMCFG?` 查询已用 `mask_key` 掩码（`at_dispatch.rs:1065`），避免从串口回显泄露；但静态存储仍为明文。
- **修复路径**: 见 §4，复用本轮新增的 `seal_secret`/`open_secret`。

### 🟡 低-中 — 模拟加密桩易被误用 — `security.rs`
- `simulate_encrypt` 为 XOR `0xAA`，`verify_auth_tag` 用非常数时间 `==`（`security.rs:173`）。
- 代码注释已声明"SIMULATION ONLY / NOT SECURE"，但 `SecurityManager::new()` 默认 `Aes128Ccm + High`，生产路径若误用会产生虚假安全感。**建议**：加 `#[must_use]`/文档强化，或令启用 `simulate_*` 时发出编译期/运行时警告。

### 🟡 低 — TRNG 失败时的已知确定性身份
- `firmware/esp32-app/src/main.rs:498`：TRNG 连续 8 次失败后 `Identity::from_secret_bytes(&[0u8; 32])`。
- 全零 seed 派生出的 Ed25519 公钥是**确定性、可被预先计算**的。注释声明"EPHEMERAL UNTRUSTED (not persisted)"；若被利用进入降级态，攻击者可伪造设备身份。属文档化降级，建议在日志中显著告警并拒绝签名类操作。

### 🟡 低 — secp256k1 非 web3 路径不校验
- `from_bytes` 的校验被 `#[cfg(feature="web3")]` 门控；未启用 web3 时仅存储不校验。仅影响无 web3 的构建，风险有限。

### ⚪ 信息 — 依赖与卫生
- 生产代码存在约 696 处 `unwrap/expect`（既有审计计数），建议批量收敛为 `?`/`ok_or`。
- 建议引入 `cargo-deny`/`cargo-audit` 扫描依赖 CVE、`cargo-machete` 清理死依赖。

---

## 4. 本轮加固落地：通用秘密封印原语

**改动文件**: `magent-core/src/wifi_pass_seal_v2.rs`（纯增量，不触碰现有 Wi-Fi 路径）

新增：
- `MAX_SECRET_PLAINTEXT = 256`、`MAX_SECRET_ENCODED_LEN`
- `SecretOpenOutcome::{Dbo2Decoded, LegacyPlaintext}`
- `seal_secret(plain: &[u8], device_key, nonce, out)` — 支持 256B 任意凭据
- `open_secret(stored, device_key, out)` — 常数时间 MAC 校验 + 旧明文回退

**验证**: 6 个新增测试（128B 长明文往返、篡改检测、错误密钥、旧明文回退、空键/空 nonce、超长拒绝）全部通过；`wifi_pass_seal_v2` 24 项、lib 整体 590 项 0 失败。

### 4.1 固件接线方案（供具备交叉编译环境时应用）

**写路径**（`at_dispatch.rs` `llmcfg_dispatch` Set 分支）：
```rust
// 用 load_device_key() 取设备密钥（已有），seal_secret 封印后存储
let key = validated.api_key.as_bytes();
let nonce = /* getrandom NONCE_LEN 字节，与 CWJAP 一致 */;
let mut sealed: HeaplessString<{wifi_pass_seal_v2::MAX_SECRET_ENCODED_LEN}> = HeaplessString::new();
if wifi_pass_seal_v2::seal_secret(key, &device_key, &nonce, &mut sealed).is_err() {
    return AtOutcome::error(7);
}
nvs_save(NVS_KEY_LLM_API_KEY, sealed.as_str(), NS)
```

**读路径**（`main.rs:1394` 构造 `Esp32DeepSeekBackend` 前）：
```rust
let stored = nvs_load_string(NVS_KEY_LLM_API_KEY).unwrap_or_default();
let mut key: Vec<u8, {wifi_pass_seal_v2::MAX_SECRET_PLAINTEXT}> = Vec::new();
match wifi_pass_seal_v2::open_secret(stored.as_str(), &device_key, &mut key) {
    Ok(_) => key,                       // Dbo2Decoded
    _ => stored.into_bytes(),           // LegacyPlaintext：旧明文，读原值并建议下次重封印
}
```
（`provision_llm_config` 的 `main.rs:985` 写路径同理改为封印。）

---

## 5. 优先级修复路线图

| 优先级 | 事项 |
|---|---|
| P0（本轮） | ✅ 通用秘密封印原语 + 测试 |
| P1（1-2 周） | 固件 LLM-key 改用 `seal_secret`/`open_secret`（§4）；`cargo-deny`/`cargo-audit` 依赖扫描 |
| P2（1 月） | `zeroize` 替换手写 `Drop` unsafe；`security.rs` 模拟桩防误用；batch 清理 unwrap/expect |
| P3（季度） | web3/AT 解析器 fuzzing（cargo-fuzz/proptest）；ESP32 task WDT；cargo-machete；第三方独立审计 |

---

**验证命令**:
```bash
cargo test -p magent-core --features std,web3,wallet,verifiable_credentials --lib wifi_pass_seal_v2
cargo test -p magent-core --features std,web3,wallet,verifiable_credentials   # 全量 590 通过
```
