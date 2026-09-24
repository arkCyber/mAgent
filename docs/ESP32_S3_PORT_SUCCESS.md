# ESP32-S3 移植成功记录（最终成功）

> **日期**: 2026-08-27
> **板子**: ESP32-S3-WROOM-1-N8R8（实际 4MB flash 变体）
> **状态**: ✅ Xtensa 固件在真实硬件上启动成功，agent 线程正常运行

## 〇、2026-09-15 复验 + 两处修复（全量测试通过）

在同一块板上重跑导入 + 全量测试，发现并修复两个阻塞问题，并新增了
S3 的一键验收脚本：

1. **分区表陈旧导致启动循环**（`flash-s3.sh`）
   - 现象：bootloader 打印 `Image length 1549152 doesn't fit in partition
     length 1048576` → `Factory app partition is not bootable` → 无限复位。
   - 根因：脚本烧的是 esp-idf-sys/PIO 构建目录根下的 `partitions.bin`。
     该文件是**早期 PIO 运行的旧副本**（factory 仅 1MB）；ESP-IDF/ninja
     只刷新 `esp-idf/partition_table/partition_table_bin`，构建根下的副本
     不会再更新。
   - 修复：`flash-s3.sh` 直接把源码 CSV（`partitions.s3.csv`，factory
     2MB）交给 `espflash --partition-table`，单一真源，不再依赖陈旧副本。
     `sdkconfig.defaults` 里的 `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`
     指向的正是这个 CSV。
2. **USB 控制台无法下发命令（S3 关键可用性缺口）**
   - 现象：S3 开发板的控制台在**原生 USB-Serial/JTAG**（D-/D+ = GPIO19/20）上，
     `CONFIG_ESP_CONSOLE_SECONDARY_USB_SERIAL_JTAG` 把日志镜像到该口，
     所以主机能看到全部日志；但 ingress 只绑 UART0（且被重映射到
     GPIO10/11），因此 `AT…` / 自然语言命令在 USB 口上**被静默丢弃**，
     除非外接 USB-TTL 到 GPIO10/11。
   - 修复：新增 `link_adapters::UsbSerialAdapter`（USB-Serial/JTAG 驱动，
     零超时非阻塞读 + 有界写）与 `EspIngressLink` 聚合类型，ingress 现在
     在 S3 上注册**两条链路**：`UART0`(idx 0) 与 `USB-JTAG`(idx 1)。
     回复按命令来源回送（`IngressSource::Rs232.port` → adapter 索引），
     所以 USB 上来的命令就在 USB 上应答。驱动安装失败时降级为仅 UART0，
     不影响启动。

### 一键全量验收

```bash
# 1) 构建 + 烧录（bootloader + partitions.s3.csv + app）
cd firmware/esp32-app && ./flash-s3.sh --port /dev/cu.usbmodem101

# 2) 全量验收（复位抓启动日志 → AT 面 → ingress → agent → 内存 → 网络）
python3 tools/s3_full_test.py /dev/cu.usbmodem101 --turns 5
```

`tools/s3_full_test.py` 覆盖：8 阶段启动 / 线程就绪 / 双 ingress 链路 /
PSRAM 堆 / AT 命令面（GMR、SYSRAM、HEAP、IDENT、UPTIME、CWSTATE、
IFCONFIG、CIPSTAMAC、SAFEMODE、TIME、BLE 的 `+CMDER:9` 负向检查与
`+CMDER` 错误码）/
ingress 往返 / agent ReAct 本地工具 / N 轮堆漂移 / Wi-Fi 链路 +
web_admin（`/`、`/api/status`、`AT+HTTPGET` 回环）。

2026-09-15 实测结果（4MB 变体，quad PSRAM）：

| 项目 | 结果 |
|---|---|
| 启动 | ✅ `boot phase 8/8: all systems nominal`，无 panic |
| 堆 | ✅ `+SYSRAM:2172387`（PSRAM ~2.17MB；internal ~130KB） |
| AT 命令面 | ✅ 全部通过（含 `+CMDER:9` / `+CMDER:7` 错误码） |
| ingress 双链路 | ✅ `[ingress] frame src=Rs232 { port: "USB-JTAG" }` |
| agent | ✅ `RESULT[...]: Tool result: temperature=48.4 C`（~4-5s） |
| 内存 | ✅ 3 轮 agent 无泄漏（2166723 → 2171687 B） |
| Wi-Fi | ✅ 联机验证见 §〇之二（热点 `arksong`，IP 192.168.144.237，RSSI -42dBm） |
| web_admin | ✅ 见 §〇之二：`/` 与 `/api/status` 均 200，出网 HTTPS 401（TLS 全通），ICMP/SNTP 正常 |

> 复验确认：**S3 的 AT/agent 交互现在只需一根 USB 线**（原生 USB 口），
> 不再需要外接 USB-TTL 到 GPIO10/11。

## 〇之二、联机复验：网络全链路 + 第三批修复（2026-09-15）

用户提供手机热点后完成联机验证，过程中又修掉两个真实缺陷：

3. **`AT+PING` 永远返回 `+CMDER:6`**（`src/ping.rs`）
   - 现象：连上热点后 `AT+PING=8.8.8.8` / `AT+PING=<自身IP>` 一律 `+CMDER:6`。
   - 日志真因：`E ping_sock: esp_ping_new_session(228): create ping task failed`。
   - 根因：`esp_ping_config_t::default()`（bindgen 生成的 `Default`）是**全零**
     结构体，而 C 宏 `ESP_PING_DEFAULT_CONFIG()` 还会填 `data_size` / `ttl` /
     `task_stack_size` / `task_prio`。`task_stack_size=0` 让
     `xTaskCreate(..., 0, ..., 0, ...)` 必然失败。**不是内存问题**：复现时内部
     DRAM 仍空闲 125KB。
   - 修复：显式写入 IDF 宏的默认值（`data_size=64`、`ttl=64`、
     `task_stack_size=4096`、`task_prio=2`），并把两条失败路径的 `rc`、
     回包数/RTT 打到日志里（此前所有失败都塌缩成一个 `+CMDER:6`，无法区分
     "任务建不起来" 与 "对端不回包"）。
   - 修复后实测：`AT+PING=<自身IP>` → `+PING: reply=4`；
     `AT+PING=8.8.8.8` → `+PING: reply=4 rtt=203us`。

4. **`AT+CWLAP` 静默返回空列表**（`src/at_dispatch.rs`）
   - 现象：supervisor 日志说 `cached scan: 48 APs visible`，但 `AT+CWLAP` 只回
     `+CWLAP:`（冒号后为空）。
   - 根因：回复行上限 `REPLY_LINE_MAX=256B`；`write!(line, "+CWLAP:{list}")`
     在整段放不下时 heapless **什么都不写**，只剩字面量前缀 —— 于是最需要它
     （排查 AP 不在范围内）的时候列表不可见。
   - 修复：逐条追加，能放几条放几条，超出用 `...` 标记。
     实测：`+CWLAP:(arksong,-43);(TJ-WIFI,-46);…;...`（正是这个列表暴露了
     用户给的热点名 `arkong` 实际是 **`arksong`**）。

5. **新增可见 AP 日志**（`src/main.rs::run_wifi_supervisor`）
   - `[wifi-sup] visible APs: arksong(-43) TJ-WIFI(-46) …`：排查 `reason=201`
     时一眼看出"是无线电收不到 AP"还是"SSID 写错了"。

### 联机实测结果（热点 `arksong`，RSSI -42dBm，IP 192.168.144.237）

| 项目 | 命令 | 结果 |
|---|---|---|
| STA 联机 | `AT+CWSTATE?` | ✅ `+CWSTATE:5,192.168.144.237` |
| 扫描列表 | `AT+CWLAP` | ✅ 48 个 2.4GHz AP（修复后可读） |
| **出网 TLS** | `AT+HTTPGET=https://api.deepseek.com` | ✅ **HTTP 401**（真实服务器应答 → DNS+TCP+TLS 全通） |
| web_admin 仪表盘 | `AT+HTTPGET=http://<ip>/` | ✅ `+HTTPGET:200 bytes=256` + HTML |
| web_admin JSON | `AT+HTTPGET=http://<ip>/api/status` | ✅ `200` + `{"version":"0.1.0","wifi_state":5,"ip":…,"ssid":"arksong","rssi_dbm":-42,…}` |
| ICMP 自身 | `AT+PING=<ip>` | ✅ `+PING: reply=4` |
| ICMP 出网 | `AT+PING=8.8.8.8` | ✅ `+PING: reply=4 rtt≈200us` |
| ICMP 网关 | `AT+PING=<gw>` | ⚠️ `reply=0`（热点不响应 ICMP，正常） |
| 时间同步 | `AT+NTPSYNC` + `AT+TIME?` | ✅ `+TIME:2026-09-15T13:24:51Z,1789478691,SNTP` |
| 掉线容错 | 热点消失后 | ✅ 按设计 `reason=201` + 退避重连，不崩不重启 |

> 热点消失（手机锁屏/关热点）后，`AT+CWLAP` 里不再出现 `arksong`，板子持续按
> 退避策略重连——行为正确。热点恢复后无需任何操作，自动重连。

### 最终全量验收（热点在线，2026-09-15 13:34）

```
$ python3 tools/s3_full_test.py /dev/cu.usbmodem101 --turns 5
RESULT: 33 passed, 0 failed
ALL CHECKS PASSED
```

新增/复跑的门禁：

| 门禁 | 工具 | 结果 |
|---|---|---|
| S3 全量验收（启动/AT/ingress/agent/内存/网络） | `tools/s3_full_test.py` | ✅ **33/33** |
| TLS 握手 soak（T4 / REQ-NET-001） | `tools/s3_tls_soak.py` | ✅ **5/5 OK**（无内存错误、无挂死、无 panic） |
| Wi-Fi 联机 + NVS 持久化 + 复位自动重连 | `tools/wifi_link_test.py` | ✅ PASS（`+CWJAP:"arksong",,0,0,DBO2`；复位后 `connected with arksong, channel 6, WPA2-PSK, rssi -38`） |
| 主机单测 | `cargo test -p magent-core --lib` | ✅ 406 passed / 0 failed |
| C61 板回归编译 | `cargo +esp check --target riscv32imac-esp-espidf --features board-c61,wifi,uart` | ✅ 0 warning |

> 注意：开机首次关联偶尔会超时一次（`connect failed: ESP_ERR_TIMEOUT`）——紧接
> 复位后热点侧可能仍持有上一次的关联状态；supervisor 会在 3s 后重试并成功，
> 属设计内的容错路径。`s3_full_test.py` 的第 6 段会在问 IP 前最多等 90s。

> 说明：`+HTTPGET` 的回复体上限 256B（`REPLY_LINE_MAX`），因此脚本只断言
> JSON 的**前段**字段（`"version"` / `"wifi_state"`）。


## 〇之三、云端 LLM 智能体：天气 / 新闻实机验证（2026-09-15/16）

### 配置

```text
AT+LLMCFG=deepseek-chat,sk-…        -> OK（写入 NVS，DBO2 无关；重启后仍在）
AT+LLMCFG?                          -> +LLMCFG:deepseek-chat,sk-6…4a31
AT+RST                              -> 重启后装载后端
```

启动日志（关键三行）：

```text
[agent] registered 3 on-device network tool(s) for the LLM: fetch_web, get_weather, get_news
[agent] installing DeepSeek LLM backend via Core-0 worker (model=deepseek-chat)
[llm-worker] started on Core 0
```

> 注意：**后端在 agent 线程启动时构建**，所以 `AT+LLMCFG=` 之后必须重启一次才生效；
> 且要等启动完成（`all systems nominal`）再写，否则 NVS 写入会失败（实测过一次）。

### 实测对话（一条 USB 线，无额外硬件）

| 输入（UART/USB 控制台） | 智能体决策（`[agent] llm tool call:`） | 回复（`RESULT[…]`） |
|---|---|---|
| `你好` / `介绍一下你能做什么` | —（纯文本模式） | `你好！我是 mAgent，一个运行在 ESP32-S3 上的嵌入式 AI 智能体……` |
| `北京天气` | `get_weather args=city=Beijing` | `+WEATHER:200 Beijing: ✨ +21°C` |
| `上海今天天气怎么样？` | `get_weather args=city=Shanghai` | `+WEATHER:200 Shanghai: ☁️ +25°C` |
| `深圳天气` | `get_weather args=city=深圳` | `+WEATHER:200 深圳: ☀️ +28°C`（中文名走百分号编码） |
| `广州天气如何？` | `get_weather args=city=Guangzhou` | `+WEATHER:200 Guangzhou: 🌦️ +28°C` |
| `用 get_news 给我今天的新闻头条` | `get_news args=limit=3` | `+NEWS:1. 8 月 70 城房价出炉：一线城市环比转涨……` |
| `把 GPIO 13 拉高` | `write_gpio args=pin=13,state=high` | `GPIO13 set to high (err=0)` |

单轮耗时约 **8–20s**（LLM 4–8s + 一次 HTTPS 取数 4–10s），每小时温度自检仍正常。
数据源：天气 `wttr.in`（免 key，一行式）、新闻 `60s.viki.moe/v2/60s`（免 key）；
`news.ycombinator.com`、`api.vvhan.com` 在本网络不可达（连接超时/DNS 失败，实测记录）。

### 最终数据（2026-09-16 复跑，热点 RSSI -44dBm）

```text
$ python3 tools/s3_full_test.py /dev/cu.usbmodem101 --turns 5
RESULT: 33 passed, 0 failed   ALL CHECKS PASSED      # 启动/AT/ingress/agent/内存/网络
$ python3 tools/s3_tls_soak.py /dev/cu.usbmodem101 --iterations 5   ->  5/5 ok
$ python3 /tmp/agent_tasks.py  （8 条自然语言任务）  ->  8/8 通过（含首条"你好"也走 LLM 文本）
```

智能体电池测试（8/8）：

```
你好                      -> 你好！我是 mAgent，运行在 ESP32-S3 上的嵌入式 AI 助手…   (纯文本模式)
北京天气                  -> +WEATHER:200 Beijing: ✨ +25°C
上海今天天气怎么样？      -> +WEATHER:200 Shanghai: 🌤️ +27°C
深圳天气                  -> +WEATHER:200 深圳: 🌦️ +28°C        (中文名百分号编码)
广州天气如何？            -> +WEATHER:200 Guangzhou: 🌧️ +29°C
用 get_news 给我今天的新闻头条 -> +NEWS:1. 8 月 70 城房价出炉：一线城市环比转涨…
介绍一下你能做什么        -> 你好！我是 mAgent，一个跑在 ESP32-S3 上的嵌入式 AI 小助手…
把 GPIO 13 拉高           -> GPIO13 set to high (err=0)
```

> 其中「深圳天气」与「上海天气」两轮出现 `E esp-tls-mbedtls: mbedtls_ssl_handshake
> returned -0x0050` → 首次 TLS 握手失败，**重试后均返回 200**：这是联网工具"有界重试"
> 生效的直接证据（修复前这两条会直接 `Task: Tool error:`）。

设备侧实时指标（`[health]` / `[latency]` 周期日志）：

```text
[health] agent alive — uptime 380072 ms, free_heap 2139187 B (low-water 2083135 B)
[latency] llm_rt:     n=10 min=364ms  avg=5188ms  wcet=6862ms
          at_dispatch:n=33 min=7ms    avg=1147ms  wcet=6288ms
          e2e_reply:  n=33 min=31ms   avg=1177ms  wcet=6324ms
          e2e_agent:  n=12 min=2434ms avg=9143ms  wcet=23219ms   <- 云端 LLM + 联网工具全程
          agent_task: n=61 min=5ms    avg=1304ms  wcet=20730ms
```

- `e2e_agent` WCET **23.2s** 正是"LLM ≤10s + 联网工具 ~15s"的最坏组合 —— 因此
  `rt_watchdog`（18s）必须在阻塞跳内逐块喂狗，`[health]` 阈值取 30s。
- 空闲配额守卫：**30s 空闲 0 次 LLM 调用**（内部自检 6 次照常运行）。


```bash
# 1) 配置 key 并重启
python3 - <<'PY'
import serial, time
s = serial.Serial('/dev/cu.usbmodem101', 115200, timeout=0.2); time.sleep(0.5)
s.write(b'AT+LLMCFG=deepseek-chat,<YOUR_KEY>\r\n'); s.flush(); time.sleep(4)
print(s.read(4096).decode('utf-8','replace')[-200:])
s.write(b'AT+RST\r\n'); s.flush(); time.sleep(1); s.close()
PY

# 2) 发任务（自然语言直接写在 UART/USB 控制台上）
printf '上海今天天气怎么样？\r\n' > /dev/cu.usbmodem101
printf '用 get_news 给我今天的新闻头条\r\n' > /dev/cu.usbmodem101
```

### 期间修掉的 6 个真问题（详见 CHANGELOG）

1. **工具没注册** → `fetch_web` 有实现但不在 agent 工具表里，LLM 看不到联网能力（天气/新闻答不出来的根因）。
2. **URL 非 ASCII** → `get_weather city=深圳` 直接拼中文进 URL，`esp_http_client` 返回 `ESP_ERR_INVALID_ARG`；改为百分号编码。
3. **空循环烧配额** → 空闲每 ~5s 的 `read sensor temperature` 自检也在调云 LLM（≈17k 次/天），且开机联网前的失败会触发跳过窗口；新增 `set_llm_local_only()`，内部自检只走本地（实测 25s 空闲 0 次 LLM 调用）。
4. **瞬时 TLS 抖动** → 热点偶发 `mbedtls_ssl_handshake -0x0050`；LLM 与联网工具各加一次有界重试。
5. **跳过窗口过长** → `LLM_SKIP_CALLS` 5→2（一次抖动原本会静音后续 5 条命令，新闻问题会被温度读数回答）。
6. **日志噪音/误报** → `rt_watchdog::feed()` 在未订阅线程刷 `task not found`（改为订阅感知的 no-op）；`[health] heartbeat stale` 阈值 15s→30s（LLM 10s + 联网工具 ~15s 属正常）。
7. **`fetch_web` 拒绝含非 ASCII 的 URL** → 中文提问时模型会给出 `…/search?q=上海到西藏最便宜机票价格`，`esp_http_client` 直接 `Error parse url`（`ESP_ERR_INVALID_ARG`）；改为**整串 URL 百分号编码**（保留 `:/?#&=` 结构、不重复编码 `%`）。实测 `fetch_web url=https://wttr.in/上海?format=3` → `+FETCH:200 bytes=24 preview=上海: 🌤️ +27°C`。
8. **不编造实时数据 / 语言一致性** → 提示词新增：必须用用户的语言回答；**工具取不到就明说取不到**。以"查询上海到西藏的最便宜的机票价格"验证：智能体用中文回答"我没有接入任何航班或票务数据源…查不到实时最低票价"，**没有编造价格**。

### 机票类查询的能力边界（实测数据源调研）

设备侧**没有可达的免 key 机票数据源**（都在板子上实测过）：

| 数据源 | 结果 |
|---|---|
| `api.travelpayouts.com`（Aviasales） | 可达但 **401 Unauthorized**（需 token） |
| `flights.ctrip.com` | **432 whaleguard block**（反爬） |
| `api.skypicker.com`（Kiwi 旧公开 API） | URL 无法被解析（`+CMDER:4`） |
| `r.jina.ai` / `www.tianxun.com` | 连接超时（网络不可达） |

因此当前智能体只会**诚实告知无法查询**。若要真正支持"最便宜机票"，需要接一个带 key 的
供应商（Travelpayouts token / Amadeus / Kiwi Tequila），实现方式与 `AT+LLMCFG` 同构：
把 key 存 NVS（例如 `AT+FLIGHTCFG=<provider>,<token>`），再加一个 `get_flight_price`
工具；数据源与 key 你提供即可接入。

### 机票查询（`get_flight_price` + `AT+FLIGHTCFG=`，2026-09-16）

设备侧**没有免 key 的机票数据源**（板子上实测：Travelpayouts `401`、携程 `432
whaleguard block`、Kiwi 旧 API URL 不可解析、`r.jina.ai`/天巡 不可达），因此与 LLM
后端同构地做成**带 key** 的能力：

```text
AT+FLIGHTCFG=travelpayouts,<TOKEN>     -> OK（写入 NVS）
AT+FLIGHTCFG?                          -> +FLIGHTCFG:travelpayouts,abcd…wxyz
# 或 Kiwi Tequila（key 走 apikey 头，需带日期）
AT+FLIGHTCFG=kiwi,<APIKEY>
```

智能体侧实测（未配置 token 时）：

```text
输入：查询上海到西藏的最便宜的机票价格
决策：[agent] llm tool call: get_flight_price args=from=SHA,to=LXA   <- 自动映射 SHA/LXA
回复：Task: Tool error: no flight provider configured — run AT+FLIGHTCFG=<provider>,<token>
```

配置 token 后的输出格式：`+FLIGHT:CNY1350 2026-10-01 MU 直飞; CNY1520 2026-10-03 CA 1转;`

同时修掉两个"工具在提示词里被静默丢弃"的真 bug（这解释了为什么模型会说"我没有
get_flight_price 这个工具"）：

1. `Tool::description` 是 `String<128>`，超过 128B 的描述经
   `try_from(..).unwrap_or_default()` 变成**空串**，而 `describe()` 会跳过空描述 →
   工具从提示词里彻底消失。现在注册失败会 `log::error!` 报出来。
2. `ToolRegistry::describe()` 容量 1024B → 2048B，且逐条完整性提交、丢弃即告警；
   `build_llm_system_prompt` 放不下工具表也会告警。
3. 新增**开机打印工具清单**：`[agent] LLM tool inventory (14 tools): …` —— 这是判断
   "模型为什么不用某工具"的唯一可信依据（让模型自己复述列表不可靠：回复受
   `max_tokens=256` 截断）。

### ⚠️ 故障恢复：console 静默 / esptool 连不上

S3 的原生 USB-Serial/JTAG 既是**烧录通道**又是（经 `UsbSerialAdapter`）**ingress 链路**，
其复位线由主机 DTR/RTS 驱动。约 10 次开/关端口 + 复位序列后出现过一次：端口仍在、
控制台完全无输出，`espflash`/`esptool`（含 `--before no_reset`）均报
`Failed to connect to ESP32-S3: No serial data received`。

**恢复**：按一下板上 **RST/EN** 键（或拔插 USB）——ROM 重新掌控外设后即可烧录；
之后同一固件连续烧录/长跑均正常，属竞态而非必然故障。

**降低概率**（已实装）：`tools/s3_full_test.py` 关闭端口前显式把 DTR/RTS 置为非断言并
短暂等待；避免短时间反复开关端口。详见 `docs/BACKLOG.md` 同名条目（含后续建议：
生产镜像可把 USB-JTAG ingress 改为可选）。



## 一、最终结论

1. ESP32-S3 移植代码（board-s3 + web_admin.rs + fetch_web）**已在真实 S3 板上烧录并启动**，agent 线程持续运行（实测温度传感器工具返回 temperature=32.6 C）。
2. 全程解决 7 类阻塞，均已修复并固化到 build-s3.sh / build.rs。

## 二、关键修复
1. **工具链**：cargo +esp（ESP-rs 工具链）；esp 默认对齐字面量，无需 -mtext-section-literals。
2. **bindgen**：BINDGEN_EXTRA_CLANG_ARGS="-target xtensa-esp32s3-none-elf"（esp-clang 默认 RISC-V）。
3. **C 交叉编译**：CC/CXX 指向 Xtensa 的 xtensa-esp32s3-elf-gcc/g++（否则 secp256k1_sys 等编成 RISC-V）。
4. **双 DROM 段修复**（build.rs）：把 .defmt.* 并入 .flash.rodata，消除 appdesc/rodata 间隙，bootloader 接受镜像（之前报 Invalid app image header 复位循环）。
5. **sdkconfig 修复**：PlatformIO 的 idf.py 只读标准名 sdkconfig.defaults；已把 sdkconfig.defaults 改为 S3 配置（4MB + partitions.s3.csv），C61 用 sdkconfig.c61.defaults（build-c61.sh 临时交换）。
6. **网络代理**：SOCKS5 127.0.0.1:10808 + PySocks（PlatformIO 下载）。

## 三、构建 / 烧录
```bash
cd firmware/esp32-app && ./build-s3.sh              # 构建 S3（sdkconfig.defaults=S3，4MB）
./flash-s3.sh --port /dev/cu.usbserial-XXX          # 烧录 bootloader+分区+app
# C61（避免 sdkconfig 冲突）：./build-c61.sh（临时交换 sdkconfig.defaults 为 C61 后构建）
```

## 四、待后续验证（非阻塞）
- ✅ **WiFi 联机**（2026-08-27）：SSID 大小写修正为 `arkSong@iPhone` 后，实机连上热点并拿到 IP `172.20.10.4`（rssi -31 dBm，稳定运行 70+ 分钟）。注：iPhone 热点需开启「最大兼容性」（2.4GHz）且热点会间歇性变不可见，属热点侧问题。
- ✅ **PSRAM quad**（2026-08-27）：free_heap ~2.18MB，4MB quad PSRAM 确认生效。
- ✅ **web_admin 主机访问**（2026-08-27）：从同网段主机**直连** `http://172.20.10.4/` 与 `/api/status` 验证成功（HTML 仪表盘 + JSON 均正常；此前 curl 空是 SOCKS5 代理干扰，直连正常）。
- ⬜ **DeepSeek / fetch_web / SNTP 时间同步**：固件就绪（SNTP 已修复为链路恢复后自动轮询），但 iPhone 热点**未提供互联网出口**（Mac 经热点出站也超时）——需热点启用蜂窝数据后才能端到端验证。

## 五、S3 vs C61 功能审计矩阵（2026-08-27）

| 功能 | C61 | S3 | 说明 |
|---|---|---|---|
| 启动 + agent/ingress 线程 | ✅ | ✅ | S3 实机已验证 |
| 本地工具（write_gpio / read_sensor 温度） | ✅ | ✅ | S3 实测 temperature≈37-41 C |
| UART 命令 / AT / 双向回传 | ✅ | ✅ | 共享源码 |
| WiFi STA | ✅ | ✅ | **S3 实机联机已验证**（IP + LINK UP） |
| DeepSeek 云 LLM | ❌ 仅本地 | ✅ | S3 专属（cfg board-s3） |
| web_admin HTTP 状态页 | ✅ | ✅ | **S3 实机主机直连访问验证成功**（含 wifi_reason 字段） |
| fetch_web 出站抓取 | ✅ | ✅ | 共享 |
| PSRAM | ✅ 2MB | ✅ quad | 实测该 4MB 板为 quad PSRAM（octal 报 not connected） |
| 安全模式 / 崩溃检测 / 健康心跳 | ✅ | ✅ | 共享，实机心跳正常 |
| Web3 钱包 | ✅ | ✅ | magent-core 共享特性 |
| BLE 管理器 | ⛔ | ⛔ | 已于 2026-09-20 从固件移除（`AT+BLE=…` → `+CMDER:9`），见 `docs/BACKLOG.md` |
| 热点容错（原因自适应退避 + SNTP 门控 + 提示） | ✅ | ✅ | 实机验证（reason=201 处理、链路恢复重连、2min 提示） |
| AT 补全（RST/CIPSTAMAC/CWJAP实时/IFCONFIG/SIGN） | ✅ | ✅ | 共享源码，S3+C61 均构建通过 |
| OTA | ✅ 8MB 双 OTA | 精简（4MB 硬件约束，暂不考虑） | 用户确认 OTA 暂缓 |

> 结论：除 OTA（4MB flash 硬件约束，暂不考虑）外，S3 已实现与 C61 同等甚至更全的功能。
