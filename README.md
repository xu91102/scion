# Scion

**Rust 编写的 Agent 研发交付编排项目：连接 GitHub/Git、独立 PR 审查、现有 CI、真实功能验收和按策略合并。**

状态：`0.0.1 / M1 read-only`。当前提供 Rust 基础架构、离线判断、GitHub 只读观察和 SQLite 任务/事件持久化；完整的 Agent → GitHub PR → CI → 验收 → 自动合并闭环尚未实现。

```mermaid
flowchart LR
    Task[任务与验收标准] --> Agent[独立工作区编码]
    Agent --> PR[GitHub PR]
    PR --> Review[独立审查]
    PR --> CI[现有 CI]
    Review --> Candidate[固定合并候选版本]
    CI --> Candidate
    Candidate --> Accept[真实流程验收]
    Accept --> Gate[策略检查]
    Gate --> Merge[受控合并与主线回归]
```

GitHub/Git 是代码、分支和 PR 的权威来源。Scion 记录任务、版本、检查和证据，在条件不满足时给出明确阻塞原因。

## 当前可以运行

需要 Rust 1.98.0，仓库的 `rust-toolchain.toml` 固定工具链。

```sh
cargo build --locked --workspace
cargo run --locked -p scion-cli -- evaluate examples/ready.json
cargo run --locked -p scion-cli -- evaluate examples/blocked.json
scripts/check.sh
```

`ready.json` 和 `blocked.json` 都是虚构输入。`evaluate` 只判断输入是否满足规则，输出包含 `mode: dry_run`、`evidence_verified: false` 和 `merge_performed: false`，不代表外部 PR 通过验收。退出码：`0` 输入条件满足，`3` 被规则阻塞，`2` 参数、文件或 JSON 错误。

目前检查：三类必要证据、非 PASS 结果、重复/缺失证据、候选版本不一致、审查者与编码者相同、空证据入口、默认关闭自动合并和所需人工确认。

## M1：真实只读观察和本地任务

```sh
mkdir -p tmp
cargo run --locked -p scion-cli -- task-create examples/task.json tmp/scion.sqlite
cargo run --locked -p scion-cli -- task-show tmp/scion.sqlite csv-export
cargo run --locked -p scion-cli -- observe xu91102/scion 1 tmp/scion.sqlite poll-001
cargo run --locked -p scion-cli -- history tmp/scion.sqlite xu91102/scion 1
```

`observe` 只向 GitHub 发出 GET 请求，并保存本地观察记录；可通过环境变量 `SCION_GITHUB_TOKEN` 提供有相应读取权限的令牌。`history` 读取缓存，不代表重新核对过 GitHub。任务保存后为 `queued`，不会启动 Agent。示例任务只是元数据，尚未与所观察的 PR 建立执行关联。

这些命令退出 `0` 表示读取/入库成功，不表示交付验收通过。所有命令都保留 `evidence_verified: false` 和 `merge_performed: false`。未配置 required checks、来源 App 不明确或读取中版本变化时，检查汇总为 BLOCKED；审查策略验证为 NOT_RUN。详细约束、最小权限和恢复步骤见 [M1 使用说明](docs/m1-observer.md)。

## 模块

| 模块 | 状态 | 职责 |
| --- | --- | --- |
| `scion-core` | 基础实现 | 候选版本、证据、状态和决策数据结构 |
| `scion-orchestrator` | 基础实现 | 离线流程判断，返回逐项阻塞原因 |
| `scion-cli` | 基础实现 | JSON 输入/输出、错误与退出码 |
| `scion-github` | M1 实现 | GET 读取 PR、实际主线、检查/审查来源与 required checks；检测读取中版本变化 |
| `scion-store` | M1 实现 | SQLite 任务规格、不可变观察事件、事件去重与新进程恢复 |
| Agent 执行器、审查执行器 | 规划中 | 隔离运行、预算与取消、独立身份审查 |
| 验收执行器、合并控制器 | 规划中 | 真实用例与制品、最后核验、受控合并 |

设计与实现顺序见 [架构](docs/architecture.md)、[首版路线](docs/roadmap.md)、[验收标准](docs/acceptance.md)。

## 与参考项目的关系

本项目参考 [CarterT27/scion](https://github.com/CarterT27/scion) 的 Rust workspace 和模块分层思路。参考项目实现独立版本控制系统；本项目首版围绕 GitHub/Git 交付流程建设，保留现有仓库和 CI。

本仓库与 Anthropic、CarterT27 没有隶属关系。参考来源、固定版本和技术取舍见 [参考说明](docs/references.md)。许可证：[Apache-2.0](LICENSE)。
Rust foundation for agent coding, PR review, CI, real acceptance and controlled merging on GitHub/Git.

团队可用 [M2 工作区协调](docs/m2-workspaces.md) 在现有仓库准备独立任务、处理目录占用与组合预检。

[M2 Agent 运行与任务 PR](docs/m2-agent.md) 说明容器执行、预算、取消和草稿 PR 的接入方式。
