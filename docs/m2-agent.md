# M2 Agent 运行与任务 PR

在现有 GitHub 仓库准备任务工作区，再显式运行 Agent。Scion 用冻结提交导出独立快照，把快照交给本机 Docker。容器结束后，宿主核对实际文件和 Git 变更，再创建提交。未挂载模型凭据的尝试可添加 `--publish` 接续推送任务分支与创建草稿 PR。

```sh
scion workspace-prepare state.sqlite my-task
scion task-run state.sqlite my-task sha256:<local-image-id> --publish
scion task-attempts state.sqlite my-task
```

任务规格继续使用 `max_attempts` 和 `timeout_seconds`。每次显式运行最多消耗一次尝试，时间预算最多一小时。容器有独立计时器，协调进程也会监控时间。记录的总耗时还包括快照准备和清理。日志记录尝试状态、耗时、退出码、提交和修改路径，模型原始输出不进入 SQLite 或宿主日志。成功提交可重放，失败后使用剩余次数重试。

从另一个终端执行 `scion task-cancel state.sqlite my-task`。运行中的进程会观察 `cancel_requested`，确认容器被移除后记录 `cancelled`。协调进程异常退出时，取消或重新运行可按尝试 ID 查找并清理原容器。Docker 不可达时保留未确认状态，待连接恢复后重试清理。

`scion task-publish state.sqlite my-task` 可在不重跑 Agent 的情况下恢复发布。任务分支使用预期为空的条件创建。已存在的分支须与候选提交一致。重复发布先核对 GitHub PR 的仓库、分支和提交，复用同一个 PR。目标分支移动后，新的发布需要基于新版本重建任务。GitHub 草稿 PR 进入团队现有审查与 CI。

## Agent 镜像

使用本机 Docker 上可信且未声明 `VOLUME` 的镜像，传入完整 `sha256:` ID。镜像需要 `/bin/sh`、`cp`、`tar`、`/usr/bin/timeout` 和可执行入口 `/scion-agent`，入口从 `/run/scion-task` 读取任务，并在 `/work` 写文件。示例镜像运行固定版本的 Codex，已有登录的操作者可显式提供供应商认证文件。示例以外层 Docker 作为沙箱，Codex 工具在容器内使用 `danger-full-access`。通过 Scion 启动镜像可获得下述隔离。

```sh
docker build -t scion-codex-agent examples/codex-agent
docker image inspect scion-codex-agent --format '{{.Id}}'
SCION_AGENT_AUTH_FILE=/path/to/provider-auth.json scion task-run state.sqlite my-task sha256:<local-image-id>
# 审查尝试提交的实际 diff 后，传入审查的 SHA。
scion task-publish state.sqlite my-task --reviewed-head <sha>
```

容器根文件系统和输入快照只读，工作目录使用 64 MiB tmpfs，输出使用限长归档。容器具有受限 CPU、内存和进程数，普通桥接网络用于访问模型。容器能读取操作者显式挂载的模型认证文件，团队应使用可信镜像，并为自动任务使用专用模型凭据。Scion 会拦截输出中直接复制的凭据，挂载凭据的候选还需审查后按 SHA 发布。容器只收到任务、快照和选填的供应商认证文件。GitHub 认证由宿主 Git 和 `gh` 单独处理。

当前支持完整工作区、普通文件和可执行位。快照阶段会拒绝符号链接、子模块和超出容量的仓库。提交阶段检查路径边界、特殊文件、硬链接和受保护的 Git/CI 配置。受管工作区保留在原版本，尝试提交由独立 Git 引用固定。SQLite 在已有 schema 2 中添加尝试表，原任务和观察命令保留各自的输出。

## 验证

```sh
docker build -t scion-agent-fixture scripts/fixtures/agent
python3 scripts/check-agent.py target/debug/scion "$(docker image inspect scion-agent-fixture --format '{{.Id}}')"
```

固定脚本镜像覆盖真实容器启动、路径阻塞、尝试上限、超时、取消和中断后的容器清理。真实模型验收应另记录模型镜像、候选提交、独立行为检查与 GitHub PR。

运行命令退出码 0 表示已提交或已发布，3 表示尝试失败、超时或取消，2 表示参数、存储、运行环境或发布错误。发布报错后可用 `task-attempts` 确认本地提交，再运行 `task-publish`。
