# 统一 Context Directives 与 Principal 身份动态切换 —— 实施记录

> **状态：已完成（IMPLEMENTED）**。本文件原是实施规划（`@skill:` / `@file:` /
> `@principal:` 三大能力）。三个阶段均已落地并通过单元测试；本文件保留为实施记录，
> 功能文档见下方链接，不再作为待办规划维护。

## 1. 背景与整体目标 (Background & Vision)

### 1.1 背景
为了将 neenee 构建为一个语法一致、扩展性强且支持动态角色调度的 AI Agent 系统，我们需要将零散的上下文注入与角色管理收优为两大核心能力：
1. **统一 Context Directives (命名空间指令系统)**：将隐式/现场注入语法规范化，支持 `@skill:xxx`、`@file:xxx` 等命名空间。
2. **Principal Identity & Profile 动态切换**：支持在对话运行期通过指令动态切换或设定 Agent 的 Principal 身份与行动策略。

### 1.2 整体目标
* **扩展 Mention 注入体系**：
  * 支持 `@skill:name` / `@skills:name`（技能注入）
  * 支持 `@file:path` / `@files:path`（文件内容现场注入）
* **支持 Principal 角色动态切换**：
  * 支持在运行期基于用户输入或命令（如 `@principal:architect` 或 `/principal <role>`）动态调用 `Agent::apply_principal_profile(&profile)`。
  * 支持更新 `AgentIdentity`（Preamble）与工具安全边界（`ToolSelection` / `OperationScope`）。

---

## 2. 详细设计与改动范围 (Detailed Architecture & Scope)

```text
neenee workspace/
├── crates/neenee-skills/src/render.rs        <-- 【模块一】@skill:xxx 前缀解析
├── crates/neenee-agent/src/
│   ├── conversation_context/
│   │   ├── skills.rs                          <-- Skill 注入处理器
│   │   └── files.rs                           <-- 【模块二】@file:xxx 文件注入处理器
│   └── agent.rs                               <-- 【模块三】Principal Identity/Profile 动态切换 API
└── crates/neenee-core/src/
    └── principal.rs                            <-- PrincipalRole 与预设 Profile 定义 (ADR-0053)
```

---

## 3. 核心功能设计 (Core Component Designs)

### 3.1 模块一：`@skill:xxx` 命名空间注入增强
* **格式**：`@skill:{name}` 或 `@skills:{name}`
* **兼容性**：向后兼容 `@{name}` 和 `skill://{name}`。
* **位置**：`crates/neenee-skills/src/render.rs`。

### 3.2 模块二：`@file:xxx` 文件现场注入机制
* **格式**：`@file:{path}` 或 `@files:{path}`（例如 `@file:src/main.rs`）
* **解析逻辑 (`conversation_context/files.rs`)**：
  1. 正则匹配消息中的 `@file:path` 模式。
  2. 进行路径安全校验（限制在当前工作空间根目录下，防止越界访问敏感文件）。
  3. 读取文件内容，自动检查文件大小（上限 50KB，超过则截断或返回提示）。
  4. 包装为隐式 Context 消息追加到请求数组中：
     ```text
     [File 'src/main.rs' loaded]
     <file content>
     [/File]
     ```

### 3.3 模块三：Principal 身份 & Profile 动态切换 (Identity Swapping)
* **原理 (基于 ADR-0053)**：
  * `PrincipalProfile` 封装了 `AgentIdentity`（身份 Preambles）、`ToolSelection`（工具选择器）与 `OperationScope`（写操作/命令权限界限）。
* **动态切换 API & 处理器**：
  1. 在 `neenee-core` / `neenee-agent` 中定义预置 Principal Roles：
     - `code`（默认程序员身份）
     - `architect`（架构师身份，侧重设计与审查）
     - `reviewer`（代码审查员身份）
     - `security`（安全审计员身份）
  2. 支持在对话流中通过指令检测（如用户输入包含 `@principal:architect` 或命令 `/principal architect`）动态触发角色切换。
  3. 在下一个 Round/Turn 生成 System Prompt 时，实时生效新的 Identity Preamble 和权限策略。

---

## 4. 实施状态 (Implementation Status)

- [x] **Phase 1: `@skill:xxx` 命名空间解析支持**（`crates/neenee-skills/src/render.rs`）
  - [x] 修改 `is_mentioned` 算法，支持 `@skill:name` / `@skills:name` / 裸 `@name` / `skill://…`。
  - [x] 单测覆盖四种提及形式。

- [x] **Phase 2: `@file:xxx` 文件现场注入模块实现**
  - [x] 新建 `neenee-agent/src/conversation_context/files.rs`。
  - [x] 实现 `inject_mentioned_files`（路径安全检查、50KB/文件上限、10 文件/轮上限、隐式 Message 追加、失败以隐藏错误注记呈现）。
  - [x] 在 agent 请求流程中挂载文件注入（与技能注入并列，无项目根目录时禁用）。

- [x] **Phase 3: Principal Profile 动态切换集成**
  - [x] 在 `neenee-core` 中定义 `PrincipalRole` 预设（`code` / `architect` / `reviewer` / `security`）。
  - [x] 在 agent 中实现动态切换（`@principal:role` 提及 + `/principal <role>` 命令）。
  - [x] 切换后 System Prompt 的 Identity Preamble 与工具边界实时生效。

- [x] **Phase 4: 自动化回归测试与集成验证**
  - [x] `cargo test --workspace` 通过。
  - [x] 技能注入 / 文件注入 / 角色切换均有单元测试覆盖（`render.rs`、`files.rs`、`principal.rs`）。

---

## 5. 验收标准 (Acceptance Criteria) — 均已达成

1. **`@skill:xxx` 验收**：
   * 输入 `请按 @skill:rust-expert 规范处理`，正确加载 `rust-expert` 技能正文。
2. **`@file:xxx` 验收**：
   * 输入 `请重构 @file:crates/neenee-skills/src/lib.rs`，自动读取并隐式注入该文件内容给 LLM。
   * 非法/超越项目根目录的路径（如 `@file:/etc/passwd`）会被安全防护拦截并报错。
3. **`@principal:xxx` 切换验收**：
   * 输入 `@principal:architect 分析项目设计`，Agent 的 Identity Preamble 与 System Prompt 实时切换为架构师角色视角。
4. **质量验收**：
   * Workspace 全部单元测试与 Clippy 检查无报错。

---

## 功能文档（当前指向）

- `@skill:` / `@skills:` / `skill://` 提及语法 —— [Glossary](docs/reference/glossary.md#input-mentions)、[Skills 说明](docs/explanation/agent-design/skills.md)、[如何编写技能](docs/how-to/write-a-skill.md)
- `@file:` / `@files:` 文件注入 —— [Glossary](docs/reference/glossary.md#input-mentions)
- `@principal:` / `/principal` 角色切换 —— [Slash commands](docs/reference/commands.md#principal)、[Glossary](docs/reference/glossary.md#input-mentions)
