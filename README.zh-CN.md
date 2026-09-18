<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">muta</h1>

<p align="center">
  <a href="./README.md">English</a> | 简体中文
</p>

<p align="center">
  通用、安全优先的 AI 智能体驾驭底座。
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## 核心特性

- **通用智能体底座 (Universal Agent Purpose)** — 超越单一代码辅助工具；提供通用的智能体架构，既支持软件工程（`developer`），也支持纯认知研究与思辨对话（`philosophist`），角色定义对称且自包含。
- **框架内灵活扩展 (Flexible Extensibility)** — 通过自包含角色包 (Role Bundles)、按需技能 (Skills)、生命周期钩子 (Hooks) 与模型上下文协议 (MCP) 实现能力即插即用，按需隔离。
- **四大接口范式原生支持 (Four Wire Protocol Paradigms)** — 坚决拒绝粗暴一刀切的“Chat Completions 伪适配”反模式；原生实现四大底层通信协议栈：**OpenAI Chat Completions**、**OpenAI Responses** (最新一代规范)、**Anthropic Messages**、以及 **Google Gemini (GenerateContent)**，100% 保真保留前沿模型（Claude, GPT, Gemini, DeepSeek 等）独有的深度思考链、提示词缓存断点与结构化输出。
- **主流模型能力完备 (Comprehensive Capabilities)** — 完备覆盖前沿模型全套能力：结构化工具调用 (Tool Calling)、深度思考推理链 (Thinking Tiers)、多模态视觉 (Vision)、实时流式输出与前缀缓存优化。
- **可扩展前端生态 (Extensible Frontends)** — 自研高性能语义化终端 (TUI `mutx`)、现代响应式 Web 应用与后台会话守护进程 (Daemon)，通过强类型契约层彻底解耦与自由驱动。
- **专注安全与资产信任 (Security-First & Trust)** — 严格的三大正交安全平面：基于内容的物理文件资产认证（SHA-256 指纹与 30 天 TTL 租期）、用户所有的物理空间目录隔离，以及防御提示词注入劫持的四级运行时危险网格。

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
3. 开始使用。在 TUI 中随时按下 `Ctrl+P`（或输入 `/`）即可发现命令与快捷键。

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
