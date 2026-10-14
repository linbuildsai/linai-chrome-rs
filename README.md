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

Rust 版聚焦**桥 + MCP + CLI** 这三个进程侧组件的单二进制化；原版的 `install`（自动写各 agent 配置）
与经验回流的高级形态（剧本入库）暂未移植，MCP 配置手工加一条即可（见下）。

## 安装

前置：Rust stable（rustup）、Chrome + 原版扩展（或本仓库 `extension/` 目录加载的扩展）。

```bash
cargo build --release
# 二进制在 target/release/linai-chrome-rs
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
cargo test                 # 脱敏规则单测（4 项）
./test/run-e2e.sh          # 协议端到端：隔离目录起测试桥（8900），9 项断言
```

e2e 覆盖：恶意 Origin 403、扩展/agent 握手、错误 token close 4001、cmd 盖章（`__k`/`sid`/`live`/`client`）、
res 按 `__k` 配对且剥离、ping→pong、event 广播、agent 断线推 `sessions` 名单、审计落盘。

## 许可

MIT。扩展与协议设计版权归原项目作者花叔（alchaincyf），见 [LICENSE](LICENSE)。
