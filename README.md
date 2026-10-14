# linai-chrome-rs

**huashu-chrome 的 Rust 改写** —— 让任何 MCP-capable AI agent 操控你自己的 Chrome，带着你全部的登录态。

> 本项目是 [alchaincyf/huashu-chrome](https://github.com/alchaincyf/huashu-chrome)（花叔，MIT License）的 Rust 重写版。
> 桥 ↔ Chrome 扩展 ↔ MCP server 三方**协议完全兼容**：原版 Chrome 扩展原样复用（`extension/` 目录），
> Rust 版桥可以直接接管原版扩展的连接，反之亦然（同一台机器同时只跑一个桥）。
> 设计动机、踩坑记录与安全模型的完整推演见 [docs/协议.md](docs/协议.md)（源自原项目）。

## 它是什么

```
Claude Code / Codex / Cursor / WorkBuddy …（任何 MCP 客户端）
        │ stdio（JSON-RPC 2.0）
        ↓
  linai-chrome-rs mcp        ← 每会话一个进程，无状态
        │ ws://127.0.0.1:8899  role=agent（token 鉴权）
        ↓
  linai-chrome-rs bridge     ← 全机单例常驻：路由 · 审计 · Origin 白名单
        │ ws://127.0.0.1:8899  role=extension（Origin 必须是 chrome-extension://）
        ↓
  Chrome 扩展（MV3，原样复用原版）  ← 快照 / 点击 / 输入 / 效果证据 / 支付闸门
```

不用 API key、不用重新登录、不用验证码农场：agent 以**你此刻这个浏览器里的身份**干活。

## 与原版（Node.js）的关系

| | 原版 huashu-chrome | linai-chrome-rs |
|---|---|---|
| 桥 / MCP server / CLI | Node.js | **Rust（tokio + tungstenite）** |
| Chrome 扩展 | MV3 JS | 原样复用（浏览器扩展无法用 Rust 写） |
| 桥协议 v1 | ✅ | ✅ 逐字段兼容（hello/welcome/cmd/res/event、`__k`/`sid`/`live` 盖章） |
| MCP 工具集 | 23 个 | 23 个（schema 与描述逐字一致，`src/tools.json` 提取自原版） |
| 审计脱敏 | 按键名递归 + 正文凭据挖掘 | 同规则移植（含手机号数字边界） |
| 配置目录 | `~/.huashu-chrome` | 同（`HUASHU_CHROME_HOME` 可覆盖，供测试隔离） |

Rust 版覆盖**桥 + MCP + CLI + install 自动化 + 经验回流（剧本草稿/入库）+ fetch 自动翻页**；
`act` 剧本形状校验与超时预算和扩展侧同源规则，`learnings` 的清洗/留底/别名归并与原版语义一致。

## 安装

前置：Rust stable（rustup）、Chrome + 原版扩展（或本仓库 `extension/` 目录加载的扩展）。

```bash
cargo build --release
# 二进制在 target/release/linai-chrome-rs

# 一键配置：探测本机已装的 agent（Claude Code / Codex / Cursor / WorkBuddy 等 20+，
# 外加自动发现 ~/.<名字>/ 下遵循 mcpServers 惯例的配置），先备份再写入
./target/release/linai-chrome-rs install
./target/release/linai-chrome-rs install --dry-run   # 只看会改哪些配置，不写
./target/release/linai-chrome-rs install --only codex
```

加载扩展（只需一次）：

```bash
./target/release/linai-chrome-rs extension
# 打印扩展目录 → chrome://extensions → 开发者模式 → 加载已解压的扩展程序
```

## 运行

**1. 起桥**（常驻，保持终端开着；或自行 nohup/launchd）：

```bash
./target/release/linai-chrome-rs bridge
# 桥已启动 ws://127.0.0.1:8899
```

**2. 配 MCP**（以 WorkBuddy/任意支持本地 MCP 的客户端为例，`~/.workbuddy/mcp.json` 等）：

```json
{
  "mcpServers": {
    "linai-chrome": {
      "command": "/绝对路径/target/release/linai-chrome-rs",
      "args": ["mcp"]
    }
  }
}
```

桥不在时 MCP server 会用排它文件锁自动抢锁拉起一个 detached 桥（与原版同机制）。

**3. 体检 / 手动发命令 / 看审计**：

```bash
linai-chrome-rs doctor                 # 一条命令查完整条链路
linai-chrome-rs call snapshot          # 手动发一条命令
linai-chrome-rs call tabs '{"action":"list"}'
linai-chrome-rs audit -n 30            # 最近 30 条操作审计
```

## 安全边界（与原版一致）

- **Origin 白名单**：浏览器发起的 WS 强制带 Origin 且不可伪造；只接受 `chrome-extension://`，
  恶意网页连桥的路在握手前就堵死（拒绝会落审计 `reject_origin`）。
- **token 鉴权**：Node/Rust 侧客户端不带 Origin，走 `~/.huashu-chrome/bridge.json` 的 token（0600，常数时间比较，随桥启动轮换）。
- **审计脱敏**：按键名递归（`text`/`value`/`password`/…只记字数）+ 正文层凭据词挖掘 + 手机号数字边界；
  `ask` 的 prompt 这类「凭据写在话里」的路也覆盖。
- **断线宽限**：扩展瞬断时在途命令等 15s（回执按 `__k` 补发配对），避免「点击已生效却报失败 → 重试变双击」。
- 支付二次确认、ref 快照防呆、效果证据等页面侧逻辑在扩展里，原样继承。

## 测试

```bash
cargo test                 # 脱敏规则 + 经验库/剧本草稿/形状校验单测（9 项）
./test/run-e2e.sh          # 协议端到端：隔离目录起测试桥（8900），9 项断言
```

e2e 覆盖：恶意 Origin 403、扩展/agent 握手、错误 token close 4001、cmd 盖章（`__k`/`sid`/`live`/`client`）、
res 按 `__k` 配对且剥离、ping→pong、event 广播、agent 断线推 `sessions` 名单、审计落盘。

## 优缺点分析：linai-chrome-rs vs huashu-chrome（Node.js 原版）

### 优点

**1. 性能与资源占用**

| 维度 | Node.js 原版 | Rust 版 | 说明 |
|---|---|---|---|
| 桥常驻内存 | **70.3MB**（实测 RSS，V8 + ws） | **7.1MB**（实测 RSS） | 同机实测，约 10 倍差距；桥是 24/7 常驻进程，这个数字乘以挂机时长 |
| 冷启动 | npx 首次要解包下载（数秒），node 启动 ~100ms+ | **单二进制 exec，<10ms** | 无运行时依赖，不经过 npm/npx 解析链 |
| 消息吞吐 | JSON.parse/stringify + 事件循环单线程 | serde + tokio 多线程 runtime，**每连接一个任务并行调度** | 多 agent 并发会话时桥侧调度不互相排队 |
| 每消息 CPU | V8 JIT 热身期慢，稳态尚可 | 原生码，**稳定无抖动** | 长会话审计重放、大批量 cmd 路由时优势明显 |

> 内存数据为 2026-10-14 同一台 macOS (Apple Silicon) 上 `bridge --foreground` 空载实测（`ps -o rss`），非估算。

**2. 分发与部署**
- **单二进制、零运行时依赖**：一个 ~3.2MB（strip+LTO 后实测）的可执行文件拷走就能跑，不需要用户机器上有 Node.js/npx，也彻底避开"npx 缓存目录漂移""node 版本不匹配""npm 网络问题"这一整类故障。
- 二进制可以静态签名/公证，企业环境审计友好；npm 包的供应链攻击面（依赖树）则完全消失——原版依赖 `ws` + MCP SDK 整棵依赖树，Rust 版依赖收敛到 tokio/tungstenite/serde 等少数成熟 crate。

**3. 类型安全与并发正确性**
- 桥的核心状态（pending 路由表、扩展多实例集合、waiting 队列）在原版里是裸 JS 对象 + 闭包约定；Rust 版被 `Arc<Mutex<...>>` + 所有权系统强制约束，**编译期排除**"两个会话的 c1 互相覆盖""连接收口竞态"这类原版踩过并修复的坑（原版注释里 8-29/8-31 的两次事故都是这类时序 bug）。
- 消息协议字段是 `serde_json::Value` 显式构造，`__k`/`sid`/`live` 盖章遗漏会在 code review 层面直接可见。

**4. 可测试性**
- 脱敏规则（redact）是纯函数 + 原生 `cargo test`，不需要 node_modules；协议端到端测试（9 项）用一个 shell 脚本 + 隔离 `HUASHU_CHROME_HOME` 就能跑，CI 里无需安装 Node。

**5. 长连接稳定性**
- tokio 的定时器/IO 由独立线程池驱动，桥的探活 ping、宽限计时器不受"事件循环被同步代码阻塞"影响（Node 版桥如果哪天混进一个同步大 JSON 操作，所有计时器一起漂移）。

### 缺点（诚实清单）

**1. 功能覆盖**
- `install` 自动化、`act` 剧本草稿入库（saveDraft/adoptDraft）、`fetch {pages}` 自动翻页已移植（v0.2）。
- 仍未移植：出厂经验 `docs/经验/` 随二进制分发的打包机制（当前开发场景从源码目录读取，`cargo install` 场景需把 `docs/经验` 拷到二进制旁）；多实例审计话术等少量边角。

**2. 生态与迭代速度**
- 原版迭代飞快（README 记录的 0.1→1.3 十几个版本、每周多次修复），且协议演进由原版主导；Rust 版是**追随者**，原版协议 v2 出来之前必须人工对齐，存在滞后窗口。
- JS 改一行存盘即生效（桥重启即换代）；Rust 改一行要 cargo build，扩展开发者同时改两侧时节奏更重。

**3. 平台覆盖**
- 当前 `libc::getppid`/SIGTERM/文件权限按 Unix 语义实现，**Windows 原生支持未验证**（原版明确支持 Windows）；需要补 winapi 分支或以 WSL 运行。

**4. 二进制体积与构建门槛**
- release 二进制 ~3.2MB（strip+LTO 实测），比"一段 JS"重；贡献者需要装 Rust 工具链，构建时间（冷缓存 ~2-3 分钟）远高于 `npm test`。

**5. 扩展无法摆脱 JS**
- 浏览器扩展必须是 JS（Chrome 平台限制），所以项目天然是"Rust + JS 双栈"，redact/compat 这类两侧共用的纯函数逻辑存在**两份实现漂移**的风险（原版是单份 JS 双端 import，这是 Rust 改写付不掉的成本，只能靠协议测试兜住）。

### 什么时候选哪个

- **选原版**：要 Windows 原生支持、要跟最新协议演进（原版迭代快，Rust 版是追随者）。
- **选 Rust 版**：桥常驻内存敏感（小内存服务器/树莓派级设备）、无 Node 环境、企业环境要单二进制审计分发、多 agent 高并发会话、或者你就是想看这套协议在 Rust 类型系统下长什么样。

## 许可

MIT。扩展与协议设计版权归原项目作者花叔（alchaincyf），见 [LICENSE](LICENSE)。
