# 首版架构

## 产品范围与当前交付

目标是把一个有明确验收标准的任务推进到可审查、可验收、可按规则合并的 GitHub PR。首个可用版本针对单个配置仓库、一个控制器实例和已有 CI，支持多个隔离编码任务；同仓库合并由一个控制器协调。

当前包含 M0 与 M1：Rust workspace、共享契约、无副作用的本地流程判断、GitHub GET 观察和本地 SQLite 任务/事件存储。尚未建立 Webhook 服务或 Agent 执行引擎。

## 模块与依赖

```text
scion-cli                       当前：JSON 判断、只读观察、任务/历史入口
    └── scion-orchestrator      当前：纯函数 assess(snapshot)
            └── scion-core      当前：Candidate / Evidence / Assessment

M1 已实现，均依赖领域契约而不进入核心：
    scion-github                PR/检查/审查/required checks 元数据，只读 GET
    scion-store                 SQLite queued 规格、观察事件和最新记录指针

后续执行适配器：
    Agent runner                Git worktree + 隔离容器中的编码执行
    Review runner               独立审查、结构化发现、版本绑定
    Acceptance runner           既定真实流程与可复核证据
    Merge controller            权威状态重读、规则确认、合并、结果记录
```

采用 Rust 2024、Cargo workspace。核心不依赖 HTTP、数据库或模型供应商。后续异步服务采用 Tokio；GitHub REST 适配采用 reqwest；首版持久化采用 SQLite。数据库、HTTP 和运行时依赖只在对应模块开始实现时加入。

GitHub/Git 保持唯一代码权威来源；SQLite 保存 Scion 自己的任务和执行状态，不复制 Git 对象库。后续存在源码取用性能瓶颈时，再单独评估 Carter 的内核和缓存方案。

## 一次交付的数据

- `TaskSpec`：目标仓库、目标分支、需求、允许修改路径、验收要求、执行预算、所有者。M1 保存不可变规格并显示 queued；M2 才执行和关联 PR。
- `Attempt`：任务的某次尝试、工作区、编码执行身份、提交、日志和退出原因。每次修复是新尝试，旧证据保留。
- `Candidate`：`repository + pull_request + head_sha + base_sha + merge_sha + policy_sha`。候选合并版本与策略版本均固定到完整提交 ID。
- `Evidence`：候选版本、类别、PASS/FAIL/BLOCKED/NOT_RUN、执行者、制品入口。三个类别是独立审查、CI 汇总、真实验收。
- `Assessment`：满足哪些输入条件、阻塞原因；M0 输出固定为 dry-run，既不认证证据，也不授予合并权限。
- `MergeReceipt`：M5 引入，保存实际合并提交、被检查的候选版本、GitHub 结果与审计记录。发布部署另走授权流程。

M0 接受调用者输入的 CI 汇总报告。M1 从实际 API 读取传统保护与 active ruleset 中的 required checks，按 context + GitHub App ID 汇总 PR head 上的检查，未知来源或配置缺失保持 BLOCKED。它不把观察自动转换为完整 Candidate 的验收收据；M3—M5 仍需验证流程定义、实际候选构建、独立审查权限和真实验收。不能用任意同名 check 或 PR 正文中的“CI 通过”替代。

首版对三类证据保守采用相同完整版本绑定。头提交、基线、候选合并提交或策略任一变化，旧证据失效；审查复用优化另行设计。

## 状态推进与失败处理

```text
queued -> coding -> pr_open -> checking -> accepting -> ready_to_merge
                                                    -> merging -> merged
任意执行阶段 -> blocked / failed / cancelled
修复后创建新 attempt，重新固定候选版本和所需检查
```

这是 M1—M5 的目标状态机。M0 实现本地综合判断，M1 保存 queued 规格和只读观察任务，不执行 coding/merging。CLI 每次 poll 使用调用者提供的 ID 去重；签名 Webhook 接收与自动调度尚未实现。后续外部副作用使用稳定幂等键。

遇到未知检查状态、失联、超时或证据缺失，进入 BLOCKED；真实失败进入 FAIL；没有执行进入 NOT_RUN。不能把 skipped/neutral 自动转换成 PASS。重试保留第一次错误和尝试次数，达到预算后停止，供用户处理。

## 执行与信任边界

1. 编码与验收代码在独立工作区和容器执行；执行者只有完成任务所需的权限，不持有合并令牌。仓库代码、PR 文本和日志都视为不可信输入。
2. 审查使用独立运行与身份，范围固定到 diff 和任务验收标准。M0 的身份字符串比较只是本地一致性检查，不能证明真实审查独立性。
3. 合并策略与验收标准从可信配置版本读取，编码 Agent 不能通过修改自己的 PR 放宽标准。涉及策略、CI、权限或数据迁移的变更需要预先配置的人工确认。
4. GitHub App 按选定仓库授权，读状态与执行写操作的能力分开。Webhook 需签名校验、大小限制、重放防护；凭据只从运行环境加载，日志脱敏。
5. 真实证据由适配器认证：核对 GitHub run/check 身份、候选提交、执行者、验收制品摘要与对应执行。调用者自填 JSON、截图或链接不能作为认证收据。

## 合并与已有 CI

仓库支持 GitHub 原生 merge queue 时，对 `merge_group` 的实际候选版本执行 CI 与验收；GitHub 负责最后的队列一致性校验。原生队列是否可用取决于仓库所属账号和方案，个人仓库不能默认假设支持。

其他仓库使用单仓串行协调器，但这只能约束 Scion 自己，不能阻止人在外部更新主线。M5 必须同时确认 GitHub 端严格的分支更新/required checks 规则有效，合并前重读 head/base，使用期望 head SHA 请求合并。发生基线变化或不能确认保护条件时，重新构建候选版本或 BLOCKED，不能依赖本地锁宣称安全。

基础 CI 文件已监听 `push`、`pull_request`、`merge_group` 和手动运行，权限为只读。它检查本仓库基础代码，不是被管仓库的完整 CI，也不是功能验收收据。后续连接项目已有工作流，保留原有 required checks。

## 公开接口和兼容

M0 JSON schema 标为实验性 v1；未知字段、错误枚举和损坏 JSON 被拒绝，不静默忽略。完整 SHA、仓库标识、非零 PR 号和证据元数据在判断阶段校验。owner 校验针对普通 github.com 用户/组织：1—39 个字母、数字或单连字符，首尾为字母数字；企业托管账号的特殊命名规则留给后续适配器扩展。语法校验不能证明仓库存在或证据真实。变更 schema 时更新示例、测试及版本。

后续持久化增加迁移和备份恢复测试；写入版本不兼容时明确拒绝。控制器崩溃后先对账外部状态，再恢复任务，避免重复建 PR 或重复合并。
