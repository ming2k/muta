<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">muta</h1>

<p align="center">
  <a href="./README.md">English</a> | 简体中文
</p>

<p align="center">
  专为软件工程打造的高性能、可控 AI 驾驭底座 (AI Harness) —— 具备语义化终端、分层执行控制、后台会话守护与自主工具编排能力。
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## 核心特性

- **AI 驾驭底座 (AI Harness)** — 为大模型执行构建严密的控制平面与确定性防护网，具备严格的执行边界、非交互安全纪律与上下文全生命周期治理。
- **语义化终端界面 (TUI)** — 自研高性能终端渲染引擎，支持实时任务进度、可折叠工具步骤与结构化代码 diff。
- **自主工具编排** — 完整的 ReAct 循环，支持 PTY 终端会话控制、文件读写、代码检索、网页搜索及 MCP (Model Context Protocol) 扩展。
- **后台会话守护 (Daemon)** — 统一管理多项目后台会话。关闭终端或断开连接不影响任务运行，随时重连并可在多会话间无缝切换。
- **定时提示与无人值守** — 通过 `/schedule` 支持 cron 周期任务及倒计时定时器，实现无人值守自动化运行。
- **持久化会话与上下文压缩** — 原子化会话持久化存储，支持上下文智能分块压缩、会话恢复与历史分叉。
- **按需技能 (Skills) 与扩展** — 模块化能力扩展，支持按需加载领域指令或在提及时自动注入上下文。

## 快速开始

### 安装预编译二进制

**macOS / Linux**:

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/muta/main/install.sh | bash
```

**Windows (PowerShell)**:

```powershell
irm https://raw.githubusercontent.com/ming2k/muta/main/install.ps1 | iex
```

### 源码编译

```bash
git clone https://github.com/ming2k/muta.git
cd muta
cargo build --release -p muta -p mutx
```

### 初次使用

1. 启动终端客户端：
   ```bash
   mutx
   ```
2. 配置模型与 API Key：
   在输入框中输入 `/models` 选择模型提供商并填写密钥。
3. 开始使用。在 TUI 中随时按下 `F1` 即可查看快捷键与帮助说明。

## 文档

- [使用指南 (How-to)](docs/how-to/) — 环境配置、功能使用与日常工作流。
- [架构与设计 (Explanation)](docs/explanation/) — Daemon 架构、渲染机制与设计理念。
- [参考手册 (Reference)](docs/reference/) — CLI 参数、斜杠命令与配置项规范。
- [架构决策记录 (ADR)](docs/adr/) — 核心技术决策文档。

## 许可证

[MIT](LICENSE)

---

<details>
<summary><b>关于名字与形象 (muta / 沐獭)</b></summary>

- **muta** 源自印欧语系词根 *mut-*（代表“变化与成长”），寓意 AI 是一个不断演进、持续成长的存在。
- 中文谐音“**沐獭**”，因此选择了一只正在洗澡的水獭作为项目形象 🦦🛁。

</details>

