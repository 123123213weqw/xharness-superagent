# XHarness SuperAgent

基于 Rust / XHarness 的主动式 Agent 产品开发项目，参考 DeepSeek SuperAgent
的产品逻辑：接收 GitHub/飞书事件，评估与授权，创建和执行任务，再将结果回复给用户。

**状态：早期开发版。已有正式可执行入口及 GitHub、飞书文本的离线协议整链路；
不是完整可部署的 SuperAgent 替代品，也不宣称已完整业务对齐。**

## 复刻什么

对齐任务、授权、角色、工具、会话控制、回复、配置和恢复等产品逻辑；
不复刻 JavaScript 语言运行时，不要求极端字符串/历史日期的全域等价。
详见 [产品范围](docs/superagent/product-scope.md)。

## 已实现的组件

- Rust 领域模型、策略与 27 个业务命令。
- SQLite 事务、配置历史、迁移、持久化 Effects 和幂等 Outbox。
- XHarness 原生会话/JSONL 接入组件、队列/followup/steer/inject、观察与关闭。
- 部分 Prompt/skills、输入准备、重试健康和崩溃恢复组件。
- 固定原版源码及组件差分验证设施。
- Application/Native 装配及20个角色应用工具注册（主角色14个、Worker6个）。
  名字已注册不代表所有分支完成；正式入口离线协议夹具可实际启动 Worker 写文件
  并经 Outbox 回复，不冒充真实外部服务或完整产品验收。
- GitHub Source 的通知/评论/PR/CI 轮询、稳定事件和 marker 回复库已接到 Native；
  [接入进展与验证边界](docs/superagent/business-github.md)。

- 正式 `xharness-superagent --config ... [--check | --once]` 入口及实际 OpenAI 兼容
  HTTP/SSE → Native → CodingToolBundle → GitHub marker 回复闭环。
  [配置、权限和验证边界](docs/superagent/business-entry.md)；外部服务由明确的
  loopback 模型/gh 协议夹具验证，没有付费调用或真实 GitHub 评论。
- [工单/线程控制](docs/superagent/business-controls.md)：重试、状态查询、重新研判、
  订阅/移除/handback 已接入同一 Native 工具路径；主14/Worker6名字已齐，完整业务分支仍未完成。
- [未完成工单恢复](docs/superagent/business-recovery.md)：等待工单重启后处理新输入、
  Worker 进程中断后的恢复 assignment、目录丢失时保留输入与可观察错误。
  只验证所列场景，不代表全部后台任务/崩溃切点和部署恢复完成。
- [业务读取](docs/superagent/business-read.md)：实际 SQL/Source/Native 读取、权限/脱敏、
  分页快照及不唤醒 Worker 的历史读取已接入两角色；完整来源分支仍未完成。
- [业务配置与提示词](docs/superagent/business-configuration.md)：块版本/冲突/来源与
  执行环境 guard、暂停/恢复、角色默认模型、Prompt preview/版本和实际团队技能。
  该页为历史接线；最新人工保存/API/UI 子集见下方管理入口，完整同步/重启仍未完成。

- [主动研判业务](docs/superagent/business-assessment.md)：候选上下文、作用域补读、规则与方向/意图校验、auto/observe、超时及取消回收；正式离线链路已验证，完整业务仍未完成。

- [跨工单事实](docs/superagent/business-note.md)：实际 Note 输入/投递，waiting 同 assignment 继续工作与 blocked 等待人工不唤醒；不改变授权。

- [飞书来源基础](docs/superagent/business-feishu-foundation.md)：bot 身份、消息/范围、成员同步、回复及受管事件流组件已实现；kind 配置与成员同步已进入正式入口；该文是历史组件里程碑，当前接线见下一项。

- [策略同步](docs/superagent/business-policy-sync.md)：正式 Main 工具 → bot CLI → 原子策略保存、版本冲突、通知、健康/退避和中断重开；7条新正式场景；这是此前独立的策略同步验收。

- [飞书文本正式入口](docs/superagent/business-feishu-live.md)：实际事件、历史补拉、正文/上下文/链接、Worker 执行与 bot 回复，新增6条正式场景（总43条）；图片/附件、完整 Source 矩阵及线上联调仍未完成。

- [文本生产化推进](docs/superagent/production-text-release.md)：SIGTERM统一回收、来源致命SQL错误退出及CI观察竞争修正；正式入口46条。允许冷切不等于已经可替换现网。

- [真实文本验收](docs/superagent/real-text-acceptance.md)：真实 GitHub Issue/追加评论 → Worker文件操作 → 回帖/重放；Messages 模型与飞书消费者分别保留验证边界。
- [文本管理入口](docs/superagent/business-management.md)：可选 loopback API/基础页面、人工版本保存/停止/暂停，正式入口现51条；完整原版 API/UI 与生产替代仍未完成。

## 尚未完成

- 完整 Assessor/Executor 协作及剩余研判/资源/生命周期分支。
- 14 个主工具与 6 个 Worker 应用工具的完整执行及生命周期。
- GitHub 完整资源/工具分支、真实外部服务联调，以及飞书图片/附件与完整多来源分支。
- 完整配置 API、管理面板、部署和回退验收。

仓库中的旧 gh-host/feishu-host 是已有 XHarness 实验，不代表上述产品已完成。
组件测试通过不等于端到端验收通过。当前状态见 [STATUS.md](STATUS.md)。

## 目录

| 目录 | 内容 |
| --- | --- |
| `crates/xharness-superagent-core` | 产品领域模型和策略 |
| `crates/xharness-superagent` | SQLite、队列、原生会话适配等应用组件 |
| 其他 `crates/xharness-*` | 随源码保留的 XHarness 运行时依赖 |
| `conformance` | 固定参考、测试夹具和差分工具；不是生产依赖 |

## 开发验证

使用 Rust 1.99.0。没有外部数据库服务依赖，SQLite 随构建提供。
所有 Rust 编译/检查/测试仅在远程服务器或 GitHub Actions 上执行，禁止本机编译。

本机可进行非编译校验：

```sh
cargo fmt --all -- --check
node conformance/restore-reference-modes.mjs
node conformance/verify-reference.mjs
node conformance/verify-upstream.mjs
node conformance/business/verify-tool-manifest.mjs
```

Git 只保留可执行位，不保留参考归档的组写权限。恢复脚本先校验全部固定源码
哈希再还原权限，不会修改源码内容或放宽校验。

远程验证入口：`bash scripts/superagent/remote-validate.sh rust`，要求已配置
SSH 别名 `WZU_Server`。工作目录独立于现有 Runtime 服务；不复制环境文件或密钥。
历史差分模式仅作研究诊断，不是全域 JS 兼容的发布门禁。GitHub CI 不自动部署、
不运行定时上游覆盖同步，不调用付费模型。

## 来源和许可证

- 产品逻辑参考：`deepseek-ai/dsh-superagent-prototype@6337fc823ce20251c8de8085948165c81aaa701e`。
- 运行时基线：`123123213weqw/xharness-runtime@2147ff270fe55c2c739e5063a5bfacefd01dc38a`，另含已审计的 XHarness 内核适配。
- Rust 代码沿用 Apache-2.0；参考源码与第三方代码保留各自许可证。
  详见 [LICENSE](LICENSE) 和 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
- 独立社区项目，不是 DeepSeek 官方发行版。
