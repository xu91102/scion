# M1：只读观察、任务和事件恢复

M1 新增两个适配模块，核心仍不依赖 HTTP 或数据库。GitHub 读取均为 GET，生产 API 固定为 `https://api.github.com`，禁止重定向；令牌只从环境读取，不进入 SQLite、任务文件或日志。所有 API 结果仅为一次观察，不授予合并权限。

## 命令和结果含义

| 命令 | 作用 | 输出模式 |
| --- | --- | --- |
| `scion evaluate snapshot.json` | 原 M0 的离线条件判断 | dry_run |
| `scion task-create task.json database.sqlite` | 保存不可变任务规格，不执行 Agent | local_task |
| `scion task-show database.sqlite task-id` | 新进程读取 queued 规格 | local_task |
| `scion observe owner/repo pr database.sqlite poll-id` | 实际 GET 读取后入库；相同 ID/内容可重放 | github_read_only |
| `scion history database.sqlite owner/repo pr` | 读取已存观察事件 | cached_history |

观察、任务和历史命令退出 0 表示 I/O 成功。即使 head_ci 为 BLOCKED，读取和保存仍可能成功。参数/网络/数据库错误退出 2；原 evaluate 的规则阻塞仍退出 3。`evidence_verified`、`merge_performed` 始终为 false。`github_observed=true` 只说明本次 observe 访问过 GitHub；缓存读回为 false。

## 令牌与权限

可不带令牌访问公开数据；限流或权限不足会报错/阻塞。需要认证时设置 `SCION_GITHUB_TOKEN`，使用有目标仓库读取权限的凭据。私有仓库通常需要 Pull requests、Checks、Commit statuses、Contents、Metadata 的读取权限；传统分支保护读取额外需要 Administration read。代码不会自动增加凭据权限，也不会修改仓库设置。

403、含糊的 404 或无法读取 active rules 不会被当作“没有约束”。仅明确的 `Branch not protected` / `Required status checks not enabled` 被视为传统检查未配置；另一来源仍必须成功读取。即使确认两处都没有 required checks，也保持 BLOCKED，不能把任意绿灯列表等同于合并条件。

## 观察与版本

观察读取 PR 的 head、状态、草稿标记和作者 ID；另读目标分支实时 ref。不能把 PR 结构里的历史 base.sha 直接当作当前主线。结束前重读 PR 与实时 ref，变化时 stable=false，head_ci=BLOCKED。

`stable` 仅指该读取窗口的代码/PR 身份一致，不承诺跨多个 GitHub API 的全局事务快照。`merge_sha` 保留 GitHub 返回语义：未合并时可能是测试合并提交，已合并时可能是 squash 等实际合并提交，不能据此认定验收过某个合并产物。

每页最多 100 条、最多 20 页；响应最多 4 MiB，每次网络请求有超时。超过限制明确失败，不返回部分通过结果。只保留必要元数据，不保存 PR/review 正文或 check 输出。

## 检查与审查

required checks 合并自传统分支保护和 active rulesets。每项必须在同一 head 上、名称和 App ID 匹配、结果唯一、状态 completed 且结论 success。缺失、歧义、无固定 App 或 skipped/neutral/未知状态不能 PASS。另分页读取同一 head 的传统 commit statuses；必需名称存在这种状态时，即使同名 check run 成功也保持 BLOCKED，因为 M1 尚不验证该来源。无关名称的传统状态不参与判定；状态读取失败会使本次观察报错。

该结果是 **head 检查信号**，不是候选合并构建或完整交付验收。M1 不验证 CI 定义是否被恶意修改，也不验证审批权限、CODEOWNERS 或 Agent 独立审查契约；原始 review 元数据已读到，历史审查中已删除的作者或提交保留为空，`review_policy` 仍为 NOT_RUN。完整收据和最终准入属于 M3—M5。

## SQLite 和事件

数据库标记为 Scion application ID、schema 1。仅初始化空的新库，拒绝含其他对象的未标记库、不同标记或未知版本，禁止自动覆盖和降级。新建文件在 Unix 上为 0600；已有文件的权限由操作者管理。

- `task_specs` 保存任务规格、预算与创建时间，同一 ID 重复同样规格可读回，内容不同报冲突。queued 代表尚未执行，不代表 GitHub 仓库/执行权限已经验证。
- `events` 保存不可变观察 JSON 和 SHA-256 指纹，`tasks` 保存每个 PR 的最新入库事件指针。两个写入在同一事务；失败全部回滚。
- 相同 poll ID、相同内容不重复添加；相同 ID、不同内容明确冲突。重放旧 ID 不会把最新指针倒回。每次新的观察使用新 ID；这不是签名 Webhook 的幂等接收器。
- `current=true` 是“最新入库”，不保证 GitHub 此刻仍相同。读取缓存不会刷新源状态，也不具有认证意义。

目前没有常驻控制器，恢复验证采用关闭进程后重新运行命令读取同一库。停止所有写入进程后可以备份数据库文件；恢复到另一位置后重新运行 task-show/history，并再次 observe 获取最新外部状态。未知后续 schema 版本需要明确的迁移方案。

## 验证边界

自动测试使用真实 SQLite 和真实回环 HTTP，仅在 GitHub 这个外部边界提供模拟响应，覆盖来源伪造、分页、读取中版本变化、无权限/无配置、非 success 结果、重复/冲突事件、事务失败回滚和新进程读取。GitHub 实际只读读取、重复 poll 与本地恢复另行保留当轮证据。

Webhook 服务、Agent 执行、审查授权、真实业务验收、最终合并候选验证和自动合并均未运行或未实现。M1 不会修改任何 GitHub 分支、PR、检查或保护设置。
