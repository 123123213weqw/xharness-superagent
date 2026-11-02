# Worker 仓库交付（显式启用、文本快照 v1）

本能力属于产品适配层，不是扩大通用 XHarness 的沙箱或复刻 JS 语言。
默认关闭；正式入口配置可增加：

```json
"repositoryDelivery": { "baseBranch": "business/product-alignment" }
```

仓库与账号只能使用启动时已验证的 GitHub Source。模型不能选择另一个仓库、
账号、主机、URL、CLI、任意 argv 或提交到默认分支。`--check` 仍然只读。
只给实际已授权工单的 Worker 增加 `repository_delivery` 编码工具，Main 不获得
此工具；原主14/Worker6应用工具和其他 Host/Runtime 默认值不变。

## 实际操作

1. `inspect`：读取显式配置的基分支 SHA、tree 和根条目，不声称已 clone。
2. 在工单工作区用现有编码工具写入、读取和验证文件。
3. `publish`：提供 `baseSha`、`files`、`title`、`body`。产品先通过已有安全
   filesystem capability 固定文件字节，再重新检查基分支和原文件模式。
4. GitHub Git Database API 创建 blobs/tree/commit，仅创建
   `xharness/ticket-<ticket>/assignment-<assignment>/<execution-name>` 分支，然后创建 **Draft PR**。
5. Worker 可用 `orchestrator_report` 的 `pr_number` 声明 PR。实际 GitHub Source
   读取 PR 后，按来源 PR 或本账号/本仓库/工作区短名称校验归属；唯一绑定、
   report、relay 共用 SQL 事务。外来 PR 或 report失败不留下半绑定。未关闭的
   自有 PR 会让工单继续开放，而不是把 ready 当成已经合并。
6. `status`：读取这个分支/PR 的 checks、reviews、inline comments；分页上限
   明示，不把只读到100条说成全量。不会设置自动合并，也不执行 merge。

每个外部写动作前都重新查询同一个 SQL owner，验证未暂停、工单仍活跃、
assignment仍running、授权仍有效；取消会取消并回收受管 CLI。
Worker 的 bash 仍 WorkspaceWrite/NetworkDeny，不接触 root GitHub 凭证。
不在 Worker 可改写的 `.git/config`、hooks、filters 中以 root 身份运行 git。

## 幂等与错误

一个 assignment 只发布一个不可变快照。重复相同请求根据远端 tree、单一parent、
工单标记和分支/PR查找恢复，不重复建分支/PR；不同快照明确拒绝，不覆盖。
PR已关闭、所有权/分支被人修改或历史有歧义时明确报错，交给用户处理。
基分支前移会要求先重新检查。取消/超时可能发生在服务端已经成功之后，
必须用 `status`/同快照重试核实，不声称远端副作用已回滚。
未被引用的 blobs/tree/commit 可能残留，由 GitHub 按其策略处理。

## v1 边界（不是全部仓库工作流）

- 1–16个常规 UTF-8 文本文件，每文件64 KiB，合计256 KiB；NUL/二进制拒绝。
- 不删除文件，不改可执行位，不替换 symlink/submodule/目录或非目录祖先。
- 拒绝绝对路径、路径穿越、`.git`元数据和 `.github/workflows/`。
- 不提供完整 checkout/clone、自由 git 命令、任意网络、远程 SSH 或依赖安装。
- 不把助手自己经 SSH 运行 Cargo 当成 Worker 已有远程编译能力。
- Checks/reviews读取成功不等于 CI完成或外部评审通过。
- 本工具是主动声明的产品能力扩展，不宣称与参考 SDK 的shell工具逐字等价。

GitHub API参数按官方 [Git trees](https://docs.github.com/en/rest/git/trees)、
[references](https://docs.github.com/en/rest/git/refs)、
[pull requests](https://docs.github.com/en/rest/pulls/pulls) 与实际 `gh api --help`
核对。正文/文件内容均用 `--raw-field`，不会将 `@文件名`解释为主机文件读取。

## 验收

离线正式入口用实际二进制、模型协议、Source、Native Worker、SQL授权、
安全文件读取、受管CLI、Outbox；只有模型/GitHub服务边界是确定性夹具。
真实服务验收另列证据，不以夹具成功冒充真实 push/PR。
详见本批验证记录；完整业务 Goal、飞书 Worker收发和六实例切换尚未完成。
