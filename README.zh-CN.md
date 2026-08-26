<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">Muta</h1>

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

- **语义化终端界面** — 自研网格+差分渲染引擎（`mutx-engine`），从零构建以替代 ratatui。保留模式网格、写时脏标记差分、宽字符所有权管理、`bce` 感知的 crossterm 后端。支持实时状态、可展开的工具步骤、结构化 diff 展示。
- **工具调用** — 完整的 ReAct 循环，支持原生与文本回退两种工具调用协议；内置 bash、文件读写、文件发现（`find_files`）、文本搜索（`search_text`）、网页搜索及 MCP 服务器。
- **定时提示** — 用 `/schedule` 按时钟调度提示：周期性 cron 任务，或倒计时 / 绝对时间的一次性定时器，让代理在无人值守时按计划运行。
- **会话 Daemon 与控制平面** — `muta` 核心 daemon 持有跨所有项目的每一个会话，`mutx` TUI 与 Web app 是同级客户端。任务不因关闭终端而中断；`muta daemon status` 提供多任务实时视图，`mutx` 内的 `/dashboard` 可切换会话而不中断工作。
- **持久会话** — 原子写入、上下文压缩、会话恢复与分叉。
- **技能系统** — 按需加载领域知识，或在被提及时自动注入。

## 快速开始

**一键安装**（macOS 与 Linux）—— 下载并校验 SHA-256 后，将预编译二进制安装到 `~/.local/bin`：

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/muta/main/install.sh | bash
```

> 可用 `MUTA_VERSION=0.35.1` 固定安装此版本，或用 `INSTALL_DIR=/usr/local/bin` 自定义安装目录。

Windows 用户可在 PowerShell 中执行：

```powershell
irm https://raw.githubusercontent.com/ming2k/muta/main/install.ps1 | iex
```

Windows 安装器目前支持 x86-64，会校验发布文件的 SHA-256，将程序安装到 `%LOCALAPPDATA%\Programs\muta\bin`，并把该目录加入用户 `PATH`；可通过 `MUTA_INSTALL_DIR` 覆盖安装位置。

**或从源码编译**：

```bash
git clone https://github.com/ming2k/muta.git
cd muta
cargo build --release -p muta -p mutx
cargo run --release -p mutx
```

首次启动后输入 `/models` 选择模型并填入 API Key（在 Kitty 键盘协议生效的终端里 `Ctrl+M` 也可以），然后直接开始对话。

第一次运行 `mutx` 会检查 Muta daemon；若尚未启动，会自动运行同目录或 `PATH` 中的 `muta`。之后每次启动会直接连接。见下文 [Daemon 模式](#daemon-模式与多任务跟踪)。

## Daemon 模式与多任务跟踪

`muta` 核心运行一个用户级**会话 daemon**，由它持有跨所有项目的每一个会话（ADR-0096）。`mutx` TUI 与 Web app 是独立客户端；会话不依赖任一前端即可持续运行：

```bash
mutx                   # 打开 TUI；需要时自动拉起 muta
muta daemon start      # 运行 daemon(默认后台)

muta daemon start --fg --public  # 前台运行，监听所有接口（TCP+token），开放给局域网客户端
mutx attach [id]       # 驱动某个 daemon 持有的会话
muta daemon status     # 一次性表格：需要注意的会话
muta daemon status --watch    # 实时表格，每次变化自动刷新
muta daemon status --json     # 原始监控帧（即中控面板的 API）
mutx dashboard         # 直接从 shell 进入全屏仪表盘
```

在 TUI 内按 **`/dashboard`** 打开会话仪表盘：一个全屏实时视图，上方是 console 区（选中会话的实时状态：当前工具、活动、上下文、进度），底部会话坞里每个会话一张卡片。回车打开只读预览；按 `a` attach 到某个会话——TUI 会先 detach 再 attach，所以你离开的会话**会在 daemon 里继续运行**。在同一界面还能 `i` 打断、`p` 发任务、`n` 新建会话。关闭 TUI 不会中断正在跑的轮次，随时 `mutx attach <id>` 接回。（`/host` 保留为隐藏别名。）

**`mutx dashboard`** 直接从 shell 进入同一个全屏仪表盘——无需先进入会话。它只把 daemon 上最近活跃的会话当作底层载体，在其之上升起仪表盘：Esc 退出，选中卡片按 `a` 则 attach 进入该会话。它会执行正常的 daemon 就绪检查，但仍需要至少一个既有会话作为载体。

daemon 默认通过 macOS/Linux 的 Unix socket 或仅当前 Windows 用户可访问的 Named Pipe 提供一条可读写的控制平面协议（创建、发提示、打断、批准、终止，外加监控流），`--public` 时同时走 TCP+token——这正是 Web 中控面板直接消费的东西。详见[如何用会话守护进程跟踪会话](docs/how-to/track-sessions-with-a-session-daemon.md)与 [ADR-0096](docs/adr/0096-unified-session-daemon.md)。

## 快捷键

| 按键 | 功能 |
|------|------|
| `F1` | 帮助（完整按键列表） |
| `F5` | `/btw` 侧线会话列表 |
| `Ctrl+Q` | 打开回合队列 |
| `Ctrl+P` | 阻塞 / 恢复回合队列 |
| `Ctrl+O` | 向运行中的回合插入输入 |
| `Ctrl+M` | 打开模型选择器（需 Kitty 键盘协议；`/models` 始终可用） |
| `Ctrl+L` | 全局视图切换器 —— 在全部表面（含选择器、仪表盘、会话列表）之间跳转，按最近使用排序；键入即模糊过滤。视图为常驻态：离开时保留滚动、选中，选择器还会保留进行中的输入草稿（ADR-0133） |
| `Ctrl+R` | 输入历史搜索 |
| `Ctrl+T` | 打开待办 |
| `Enter` | 发送消息 |
| `Tab` | 确认斜杠命令 / `@path` 补全的高亮项（Esc 关闭后可重新唤出） |
| `Ctrl+B` | 光标向左移动一个字符（readline backward-char） |
| `Ctrl+C` | 复制 → 中断 → 关闭弹窗 → 清空 → 退出 |
| `Ctrl+V` | 粘贴剪贴板内容 |

完整权威列表在 TUI 内按 `F1` 查看。

## 常用命令

| 命令 | 说明 |
|------|------|
| `/schedule <when> <提示>` | 按 cron（周期性）或倒计时 / 绝对时间（一次性）调度提示 |
| `/compact` | 压缩上下文以释放空间 |
| `/sessions` | 浏览和打开历史会话 |
| `/usage` | 跨会话使用统计：每日 token、各模型用量、最近请求日志（不随会话清理消失） |
| `/export` | 将对话导出为 Markdown |
| `/mcp` | 查看 MCP 服务器连接状态 |

详细架构、指南和参考文档见 [docs/](docs/)。

## 许可证

[MIT](LICENSE)
