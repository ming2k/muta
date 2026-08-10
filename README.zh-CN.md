<p align="center">
  <img src="./assets/logo.png" alt="neenee logo" width="256">
</p>

<h1 align="center">妮妮</h1>

<p align="center">
  <a href="./README.md">English</a> | 简体中文
</p>

<p align="center">
  一个基于 Rust 的 AI 编码助手，提供语义化终端界面、工具调用、按需技能和定时提示。
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="#"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## 特性

- **语义化终端界面** — 自研网格+差分渲染引擎（`neenee-tui-engine`），从零构建以替代 ratatui。保留模式网格、写时脏标记差分、宽字符所有权管理、`bce` 感知的 crossterm 后端。支持实时状态、可展开的工具步骤、结构化 diff 展示。
- **工具调用** — 完整的 ReAct 循环，支持原生与文本回退两种工具调用协议；内置 bash、文件读写、grep、glob、网页搜索及 MCP 服务器。
- **定时提示** — 用 `/schedule` 按时钟调度提示：周期性 cron 任务，或倒计时 / 绝对时间的一次性定时器，让代理在无人值守时按计划运行。
- **会话 Daemon 与控制平面** — 一个用户级 daemon 持有跨所有项目的每一个会话：任务不因关闭终端而中断，你可以随时随地观察或驱动它们——`neenee status` 提供多任务实时视图，TUI 内 `/host` 切换会话而不中断工作，可读写的控制 API（创建 / 发提示 / 打断 / 批准 / 终止）走本地 socket 或 token 保护的局域网端口——Web 面板消费的正是这套协议。
- **持久会话** — 原子写入、上下文压缩、会话恢复与分叉。
- **技能系统** — 按需加载领域知识，或在被提及时自动注入。

## 快速开始

**一键安装**（macOS 与 Linux）—— 自动下载预编译二进制到 `~/.local/bin`：

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/neenee/main/install.sh | bash
```

> 可用 `NEENEE_VERSION=0.22.1` 指定版本，或用 `INSTALL_DIR=/usr/local/bin` 自定义安装目录。

**或从源码编译**：

```bash
git clone https://github.com/ming2k/neenee.git
cd neenee
cargo run --release
```

首次启动后按 `Ctrl+M` 选择模型并填入 API Key，然后直接开始对话。

第一次运行 `neenee` 会自动拉起会话 daemon（一次性冷启动，之后每次启动即连）。见下文 [Daemon 模式](#daemon-模式与多任务跟踪)。

## Daemon 模式与多任务跟踪

neenee 作为客户端连接到一个用户级**会话 daemon**，由它持有跨所有项目的每一个会话（ADR-0096）。会话不依赖 TUI 即可持续运行，多个客户端可以同时驱动或观察：

```bash
neenee                   # 接入 daemon（首次使用自动拉起）
neenee serve             # 前台运行 daemon
neenee serve --detach    # …或后台运行
neenee serve --expose    # 同时通过 TCP+token 开放给局域网客户端
neenee attach [id]       # 驱动某个 daemon 持有的会话
neenee status            # 一次性表格：需要注意的会话
neenee status --watch    # 实时表格，每次变化自动刷新
neenee status --json     # 原始监控帧（即中控面板的 API）
```

在 TUI 内按 **`/host`** 打开中控面板：实时显示 daemon 上的所有会话，每行带状态、选中行带预览。回车切换到某个会话——TUI 会先 detach 再 attach，所以你离开的会话**会在 daemon 里继续运行**。关闭 TUI 不会中断正在跑的轮次，随时 `neenee attach <id>` 接回。

daemon 默认通过 Unix socket 提供一条可读写的控制平面协议（创建、发提示、打断、批准、终止，外加监控流），`--expose` 时同时走 TCP+token——这正是 Web 中控面板直接消费的东西。详见[如何用会话宿主跟踪会话](docs/how-to/track-sessions-with-a-session-host.md)与 [ADR-0096](docs/adr/0096-unified-session-daemon.md)。

## 快捷键

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 |
| `Tab` | 接受斜杠命令 / `@path` 补全 |
| `Ctrl+M` | 打开模型选择器 |
| `Ctrl+T` | 打开待办 |
| `Ctrl+B` | 光标向左移动一个字符（readline backward-char） |
| `Ctrl+C` | 复制 → 中断 → 关闭弹窗 → 清空 → 退出 |
| `Ctrl+V` | 粘贴剪贴板内容 |

## 常用命令

| 命令 | 说明 |
|------|------|
| `/schedule <when> <提示>` | 按 cron（周期性）或倒计时 / 绝对时间（一次性）调度提示 |
| `/compact` | 压缩上下文以释放空间 |
| `/session list` | 浏览和恢复历史会话 |
| `/export` | 将对话导出为 Markdown |
| `/mcp` | 查看 MCP 服务器连接状态 |

详细架构、指南和参考文档见 [docs/](docs/)。

## 许可证

[MIT](LICENSE)
