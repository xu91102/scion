# 参考来源与架构取舍

## CarterT27/scion

参考固定版本：[`e0ffadf9c755f1b99812df5beffcabdebcb38c19`](https://github.com/CarterT27/scion/tree/e0ffadf9c755f1b99812df5beffcabdebcb38c19)。参考其 Rust 2024 workspace、纯领域模块与客户端/存储分离、版本绑定、增量处理和可复核测试的思路。

该参考实现以独立版本控制服务器作为代码权威，使用 CockroachDB 与 S3；其 v1 将审核系统、CI 编排、Git 协议兼容和双向同步列为范围外。本项目首版的目标是 GitHub/Git 上的研发交付编排，因此采用自己的任务和证据契约，代码权威仍保留在 GitHub/Git。

本次基础代码为新写实现，没有引入参考仓库源文件或第三方测试样本。未来复用源代码时保留对应许可证、版权与归属记录，不能把第三方组件描述为本项目原创。

## 官方设计与平台资料

- [Anthropic：Redesigning source control](https://www.anthropic.com/research/redesigning-source-control)：面向在线 Agent 的服务端能力设计。
- [GitHub：管理 merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)：队列可用条件、候选版本与 `merge_group` 工作流。
- [GitHub：Securely using pull_request_target](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target)：不可信 PR 代码与高权限执行上下文的隔离。
- [GitHub 注册入口](https://github.com/signup)与[创建仓库说明](https://docs.github.com/en/repositories/creating-and-managing-repositories/creating-a-new-repository)：普通 owner 格式、仓库名称允许字符与 100 字符上限。

平台能力可能变化，开始实现外部适配时再次核对官方契约及目标仓库实际设置。仓库名称 Scion 仅为本开源项目名称，不表示与 Anthropic 或上述参考项目存在隶属关系。
