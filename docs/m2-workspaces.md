# M2 工作区协调

团队可以在现有 GitHub 仓库使用 Scion 安排并行任务。协调器准备一个共享 Git 对象库，为每个任务固定起始提交并分配独立 worktree。团队成员可在返回的目录里使用现有编辑器、Agent CLI 和 Git。远端 PR 与 CI 继续使用团队原有入口。

## 准备两个任务

使用支持 `git worktree`、`git sparse-checkout` 和 `git merge-tree --write-tree` 的 Git。仓库的 `origin` 应指向登记的 GitHub 仓库，Git 沿用本机已有的 HTTPS 或 SSH 认证。受管状态目录和数据库应位于所有 checkout 之外，其父目录需存在。现有状态目录必须是本机私有目录。

在仓库外保存 `project.json`。把路径和仓库换成自己的值。

```json
{
  "repository": "example/project",
  "source_checkout": "/path/to/existing-checkout",
  "state_root": "/path/to/scion-managed"
}
```

以 [任务样例](../examples/task.json) 为模板准备两个 JSON。每个任务使用唯一 ID、同一目标分支和独立的 `allowed_paths`。

```sh
scion project-register project.json state.sqlite
scion task-create first.json state.sqlite
scion task-create second.json state.sqlite
scion workspace-prepare state.sqlite first
scion workspace-prepare state.sqlite second --sparse docs
```

`--sparse` 是可选的读取目录配置。Git cone 模式也会保留顶层及必要父目录文件。默认完整检出，`allowed_paths` 独立决定允许的修改范围。稀疏工作区先创建，再按配置裁剪本地文件，共享对象库按缺失提交补取。当前无按目录下载对象的承诺。

两个任务分别在返回的 `result.workspace.path` 中修改并提交本地分支。若路径存在父子或相同目录关系，第二个请求返回 `BLOCKED`，列出占用任务与两侧路径。共享 lockfile 或协议文件可以交由一个单独的任务处理。

## 检查、恢复和释放

```sh
scion workspace-show state.sqlite first
scion workspace-preflight state.sqlite first second
scion workspace-recover state.sqlite
scion workspace-release state.sqlite first --confirm-stopped
```

预检按参数顺序组合，使用目标分支的实时 SHA 与每个工作区提交。它检查相对任务起点的修改范围，再让 Git 模拟合并。报告里的 `CLEAN` 只表示该顺序的文本合并可行。输入提交或任务状态变化后需要重新预检，真实测试和 CI 继续作为各自独立的检查。

- `CONFLICT` 提供 Git 报告的路径，部分目录冲突可能只有冲突状态。
- `DIRTY` 提示先处理已跟踪变更和 Git 报告的未跟踪文件。正常 Git ignore 规则适用。隐藏修改的索引标记也会触发此状态。
- `STALE` 表示读取窗口中目标或工作区输入变化。
- `BLOCKED` 表示目录占用、越界或工作区状态等约束不满足。子模块改动或已初始化的子模块需要人工检查。
- 退出码 0 表示操作完成或预检干净，3 表示有上述诊断，2 表示参数、存储或 Git 操作错误。

准备失败会保留占用。重开 CLI 后用 `workspace-show` 查看状态，再用 `workspace-recover` 核对并续接同一工作区。若部分目录的身份或内容无法确认，先检查并保留文件，协调器不会接管未知目录。释放前确认 Agent 已停止，提交或移走脏文件。释放后保留本地分支和工作区，新的占用使用新任务 ID。

同一个本机状态库协调使用它的任务。外部 checkout 与 GitHub 写入仍可改变主线。受管 Git 使用原始提交对象，关闭自动执行的监视器，并拒绝自定义执行驱动或传输设置。工作区是协作隔离，自动执行 Agent 所需的沙箱属于后续阶段。

## 升级和回退

使用 M1 数据库前先停止所有进程，再执行 `scion state-upgrade state.sqlite`。操作校验源结构、保存并检查 `state.sqlite.schema1-backup`，再事务升级到 schema 2。普通命令会提示升级，已有 M1 的任务与观察输出维持相同 JSON 格式。新数据库直接使用 schema 2。

若升级中断且已留下备份，先保留该文件并核查源库版本。若仍是 schema 1，把该备份移到安全位置后可重新执行。回退时停止写入，将 schema 2 文件和受管目录留作恢复依据，在原路径恢复 schema 1 备份并使用 M1 二进制。升级之后的协调记录需单独保留，工作区文件与 Git 提交留在受管目录中。工作区目录身份与数据库的规范绝对路径绑定。

`scripts/check.sh` 运行真实 Git 和 SQLite 的双进程场景。它使用临时仓库和本地远端，覆盖并行占用、内容冲突、范围校验、稀疏隔离和恢复。运行 `python3 scripts/check-workspaces.py target/debug/scion /path/to/scion-m1` 可额外对比旧程序生成的数据库和备份。
