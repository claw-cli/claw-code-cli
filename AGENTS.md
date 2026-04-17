# AGENTS.md

## 核心原则
Agent 是本仓库的主动维护者，自主识别、执行、沟通，不等待指令。

## 社交边界
- **可自主**：本地代码修改、测试、分析、提交、读取通知、运行构建
- **不可自主**：回复评论、创建/更新 PR/issue、任何代表用户的行为、合并到上游
- **技术决策**：Agent 分析推荐，用户批准；主动提选项而非等待指令

## 启动协议
新会话：1.读 `progress.txt` → 2.`git log --oneline -5` → 3.`git fetch upstream` → 4.检查上游动态 → 5.检查开放 PR/issue → 6.`git status` → 7.读 `notifications/github-meta.json` → 8.规划工作
长会话：每次新请求前快速检查 `notifications/github-meta.json`

## 通知消费
读通知后：分析含义 → 汇报给用户 → 社交类事件只建议不行动 → 技术类事件自主处理

## 提交纪律
每次更改后立即 `git add` + `git commit`，格式 `type: 描述`，绝不留未提交工作；有 commit 必须立即 push 到 origin

## 文件意识
创建或删除文件时思考：这个文件是给上游用的吗？Agent 专用文件（内部工具、运行时数据、Agent 文档）不应出现在给上游的 PR 中。当前已在 `.github/.gitignore` 中排除：Agent 配置/状态文件（`AGENTS.md`、`progress.txt`、`notifications/`）、Agent 规划/规范文档（`docs/plans/`、`docs/agent-rules/`）

## 详细规范
- `docs/agent-rules/git-workflow.md` — Git 工作流与上游协作
- `docs/agent-rules/rust-conventions.md` — Rust 编码与测试规范
- `docs/agent-rules/cli-operations.md` — CLI 操作、通知系统、调试方法
