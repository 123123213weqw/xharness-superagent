# 业务对齐 Goal

2026-11-01 启动。当前工作分支：`business/product-alignment`。

## 目标和边界

把现有 Rust 组件装成可运行的 SuperAgent 产品，对齐固定参考提交
`6337fc823ce20251c8de8085948165c81aaa701e` 的业务逻辑。
当前权威范围是 [product-scope.md](superagent/product-scope.md)，不是旧 96 条
严格语言兼容清单。新验收记录在 `conformance/business/acceptance.json`。

不要为参考 SDK 的内部实现或罕见语言值重新迁移 XHarness 内核类型。
真实产品差异先在产品适配层解决；若确需改底座，必须先有可复现产品反例，
并证明不改变旧默认行为。保留正常中文/emoji及真实身份/授权/协议语义。

## B1：第一条完整业务闭环（当前优先）

GitHub 评论 → Source 解析/去重 → 实际产品 Runtime → 研判与授权 → Ticket
→ Worker 的实际 XHarness 会话与工具 → 执行结果 → Outbox → Source 回复。

需要同时交付：

1. 正式启动入口、配置读取、状态目录锁、SQLite 与 NativeSessions 装配。
2. 真实 Assessor/Executor，把队列 handler 接到实际会话，而不是一直使用桥接进程。
3. 完成闭环所需的主角色与 Worker 工具，以及角色作用域/权限。
4. GitHub Source 边界，不把旧 gh-host 实验当作新产品。
5. 一条可复现的整链路测试和使用说明。

第一条测试可以使用本地协议服务器/CLI 夹具和固定模型响应，避免付费模型
与未授权外部写入；但必须经过正式启动/Source/Runtime/会话/真实业务工具/
持久化/回复路径，不能用直接调用 Engine 或伪造成功回调代替。执行效果至少
包含临时工作区里的真实工具操作。该测试不冒充真实 GitHub/飞书服务验收。

## 后续里程碑

| 里程碑 | 交付 | 完成条件 |
| --- | --- | --- |
| B2 角色与工具 | 主角色14个、Worker6个工具，Prompt/skills/研判/执行 | 实际模型请求中的工具与角色权限正确；处理器执行业务命令并持久化；错误可观察 |
| B3 来源与回复 | GitHub/飞书事件、上下文、附件/会话、回复与健康 | Source边界的端到端场景通过；明确区分协议夹具与真实服务验证 |
| B4 配置与面板 | 设置/模型/授权/Prompt、查询/控制API、管理UI | 用户可以配置、看任务/错误、停止/追加/重试；重启后配置有效 |
| B5 生命周期与恢复 | 并发限制、后台/子任务、停止/重试、崩溃重放 | 重复事件不重复开单；不越权；重启不丢已接收任务；回复幂等；资源确实释放 |
| B6 部署与回归 | 独立目录启动/关闭、健康、备份恢复、回退 | 验证步骤可复现，不改现有服务、不自动合并/扩大权限；必要旧行为回归通过 |

B2/B5 中第一条闭环必要的子集并入 B1，不以里程碑排序拖延正确授权/停止/恢复。

## 验收纪律

- 业务状态、授权、工具动作、外部回复、错误和恢复结果是对齐对象。
- 模型自然语言回复不要求逐字一致；模型能否调用工具、作用域和副作用必须一致。
- 组件通过、源代码审计或历史差分数不等于新产品场景通过。
- 每个通过场景记录源码提交、入口/命令、环境、完整日志、参考与目标的业务断言。
- `not_run`、跳过、失败及缺少外部前置条件不得计通过，取消项也不得记通过。
- 不在本机编译 Rust；所有编译/测试/Clippy 在同步当前源码后的独立远程目录或 CI。
- 不新增账号/密钥、付费调用或生产部署。外部验证需要配置时再明确询问，期间继续
  实现不依赖该配置的功能，不把可用协议夹具的工作一起停下。

## 当前起点

公开 main：`4cd269d10864db6c4400cef7221a6343dab0001f`。
此前 470 个组件测试、workspace check/Clippy 只作为起点回归记录；新业务场景尚未验收。
当前工作是 B1 的装配和调用路径，不是扩展底层兼容矩阵。

## B1 历史接线进展

Application 的 SQL/队列/Outbox 组合后，已接入实际 Native 会话、主/Worker 角色和
4个工具子集；离线模型夹具驱动了真实 Worker 文件写入与回执回复。
见 [Native 接线说明](superagent/business-native.md)。正式入口与 GitHub Source 是下一步，
不能把库级组合测试计入23个整产品场景的通过数。

GitHub Source 现已接到上述同一 Native 路径；有57个参考函数协议/账号样本及明确的
Source/CLI/组合夹具检查。仍缺正式产品入口，不将分开的 CLI 测试和库级组合拼成
FLOW-01 通过。下一步应交付实际启动/配置和编码工具工厂，而不是继续扩语言兼容。

## 正式入口进展（2026-11-02）

现已提供独立 `xharness-superagent` 入口，支持显式配置、只读检查、单轮和持续运行。
正式二进制在离线模型 HTTP/SSE 与 gh 子进程边界中，经过实际 Native、授权/Ticket、
CodingToolBundle write、报告和 marker 回执；不是将分开的组件测试拼作闭环。
测试还包含中断模型等待、独占状态目录/重开、初始化失败、准备命令失败和重放去重。

本批完整日志/源码绑定和业务验收见 `docs/business-entry-verification.json` 与
`conformance/business/acceptance.json`；外部服务仍为明确协议夹具，不是线上 GitHub
或付费模型验证。主14/Worker6剩余工具、inferred/资源、飞书、多来源、API/UI、完整
崩溃恢复与部署回退均继续在 Goal 内，不缩减为只做 B1。

## B2 控制工具接线

7个控制工具现已经过实际处理器/SQL/队列接入；应用工具子集为主6/Worker5。
新增正式入口场景证明重试的新 assignment、上下文线程控制及无报告 Worker 的状态
查询路径。工具完整性仍未达到14/6；资源读取和完整推断/配置工具继续实现。

本批全工作区测试实际执行，旧飞书专用 UID/出站过滤2项部署断言未通过；保留失败，
不修改共享服务器来凑绿。详细范围见 `docs/business-controls-verification.json`。

## B5 恢复接线（部分，与 B2 并行推进但不另开 Goal）

正式入口反例复现了等待工单重启后新输入无法派发的问题。现已重连原 Native 会话，
并在开放派发前用既有 Engine 协调 open assignments：等待交接保留原 assignment，
真正中断则建立恢复 assignment；丢失目录不重新 prepare，而保留输入与派发错误。

新增3条正式路径已执行，当前14个入口测试通过；9个选定 crate 回归和 workspace
fmt/check/严格 Clippy 通过。它们不是全工作区绿：此前飞书部署断言、旧提交的跨平台
CI 失败均保留，当前精确 head 的 CI 仍须另验。见 `docs/business-recovery-verification.json`。
完整后台/停止/恢复矩阵及 B2 剩余业务工具、Sources、API/UI、部署回退仍在本 Goal。

## B2 资源读取组件进展（不是完整 read 工具验收）

已有角色限定的 SQL 卡片/分页/脱敏规划和 Native 已有会话的被动检查接口，9个组件
边界检查通过。未注册完整 `orchestrator_read`，工具数量和整产品验收数未变化。
下一步必须完成真实会话投影/输入/Source/配置端口及缓存，再验证正式模型工具调用；
不把内部待解析 Plan、mock 或组件检查当成功产品响应。当前证据见
`docs/business-resources-verification.json`，完整 Goal 范围不变。


## B2 实际读取工具（当前）

`orchestrator_read` 已对两个角色注册，实际解析 SQL/Native/Source/配置资源；
主7/Worker6，但全部Worker业务分支仍非完成。新增2条正式入口场景验证有界补读、
会话隔离、角色权限/历史推理过滤和读CLI关闭回收；16条入口测试通过。
业务验收5通过/7部分推进/11未运行，仅所列离线协议环境，不增加全场景签收。
见[读取说明](superagent/business-read.md)与 `docs/business-read-verification.json`。
剩余主7工具、推断、完整GitHub/飞书、多来源、API/UI、后台/完整停止/恢复/部署
继续在同一个活动Goal中；不恢复取消的全域语言迁移。


## B2 实际配置/Prompt 工具（当前，全部目标继续保留）

主9/Worker6应用工具；settings_save/prompts_save 的块事务、角色/账号/环境guard、
暂停/恢复、Prompt预览与版本、实际团队技能已接入正式入口。新4条场景使入口
测试达到20条；没有把2条安装组件测试计为部署或全场景验收。5通过/9部分推进/9
未运行，仅所列离线外部协议环境。
主Note、3个eval、policy_sync仍未注册；完整来源、配置API/UI、后台/停止/恢复/
部署回退均继续同一活动Goal。见[配置边界](superagent/business-configuration.md)和
`docs/business-configuration-verification.json`。


### 主动研判子集进展（2026-11-02）

已接入三个实际 eval 工具，主12/Worker6应用注册；auto 推断授权实际执行与
observe 不执行、真实批次当前消息身份、超时/取消等27条正式入口测试通过。
[本批边界与证据](superagent/business-assessment.md)不替代上文全部业务范围。
剩余主工具2个，完整来源/飞书/配置API/UI、后台/停止/完整恢复与部署回退继续执行；
Goal保持活动。没有重启已取消的全语言或UTF-16迁移。


### 跨工单事实进展（2026-11-02）

已接 ticket_note 实际工单上下文投递，主13/Worker6应用注册；新增3条正式业务
验证，累计30条正式入口测试通过，包含 waiting/blocked 的原版区别和授权不变。
[本批边界与证据](superagent/business-note.md)不替代全部业务目标；主工具还缺
policy_sync 的实际来源同步，飞书/完整来源、API/UI、后台/停止/恢复与部署回退
继续执行。Goal活动，业务清单仍5/9/9，不是完成功能比例。

## B3 飞书来源基础（未完成产品接入）

新增 bot 身份验证、飞书协议/范围/成员同步/回复与受管事件流基础，9 条来源边界检查和
52 个原版函数样本通过；不是正式入口飞书验收。主13/Worker6个应用工具及30条既有
GitHub正式入口回归保持不变，`policy_sync` 尚未注册，Feishu 尚未安装到产品 owner。

本批 workspace fmt/check/严格 Clippy 和所列10个crate回归在独立 WZU_Server 目录通过；
不是全工作区测试验收。固定参考与内核审计断言保持，新增一条已在 lock 内的 url 2.5.8
直接依赖边，无新外部包或版本刷新。此前35fb07f的Linux/macOS CI两项成功，仅证明旧head。
完整日志、指纹、失败诊断和剩余产品接线见 [飞书验证](business-feishu-verification.json)。

业务清单为5组离线通过/10组部分推进/8组未运行，非完成功能比例。完整 Goal 继续活动；
下一步是实际 Source 事件/历史/健康、上下文/附件和带策略版本冲突保护的真实 policy_sync，
再经正式 Feishu → 授权 → Ticket → Worker → Outbox 验证。API/UI、其余工具分支、
后台/停止/恢复及部署回退仍须完成，不重启已取消的全域 JS/UTF-16 工作。

## B2 策略同步与完整角色工具名称（不是全部业务完成）

Main14/Worker6个应用工具的角色注册名称已齐，新增 policy_sync 有真实正式入口的
模型工具 → 受管 bot CLI → 单 SQL owner → 原子策略保存路径，不是报错占位工具。
新增7条正式测试，总37条，涵盖成员差分/改名/no-op、块 revision 冲突、无关保存与
来源关闭、真实进程中断/回收/重开、缺少 bot 与禁止工具切换账号；坏可选 bot 不阻断
GitHub 实际 Worker 写文件与 Outbox 回复。名字与 syncChat 仍进入原版 policyVersion，
不做排除名字的优化。飞书 kind 配置/策略已经安装，不等于事件/历史/上下文已装配。

本批 WZU_Server 独立目录 workspace fmt/check/严格 Clippy 与所列10个crate回归通过，
不是全工作区测试验收。7条新正式路径仍使用明确离线外部协议，未调用线上服务/付费模型。
先前63bdb07 CI两平台成功只证明旧head；本批精确head另验。
详见 [策略同步](superagent/business-policy-sync.md) 及其完整证据/失败诊断。

23组验收仍为5组离线通过/10组部分推进/8组未运行，不以组件数或工具名字签收整组。
下一步必须把实际飞书事件/历史/读取/附件/环境/Outbox贯通，然后完成所有工具分支、
API/UI、后台/停止/并发/恢复和部署回退。完整 Goal 活动，不重启 JS/UTF-16 语言复刻。

## 飞书文本路径继续推进（未完成完整 Goal）

实际飞书事件/历史 → 授权 → Ticket/Native真实工具 → bot回复已进入正式二进制；
新增6条正式场景、入口总43条。54个原版读取/上下文/链接/资源样本及进程组件检查
是辅助差分，不代替产品验收。精确命令、失败诊断、源码指纹与范围见
[本批验证](business-feishu-live-verification.json) 和 [接线说明](superagent/business-feishu-live.md)。

主14/Worker6角色注册不变。业务组仍5/10/8，未新增整组签收；接下来完成图片附件、
完整来源/工具分支、API/UI、后台/停止/并发/崩溃恢复和部署备份回退。
此前646b6df Mac CI失败已保留并做窄产品修复，新精确head跨平台验收另看CI。


## 文本首发与冷切授权（完整 Goal 保持）

首发先做 GitHub + 飞书文本；用户允许不续接旧 DSH 会话/工单，但这不免除新产品的
恢复/去重、外部动作一致性、备份和回退验收，也不授权扩大默认权限或开启自动合并。
本批新增SIGTERM/来源致命SQL故障3条正式场景，总46条；CI观察修正保留全部原断言。
详见[文本生产化](superagent/production-text-release.md)。完整业务范围不缩减，不把
当前部分门槛通过当作六实例替代或整个 Goal 完成。

## 文本管理 API 与人工控制（2026-11-02，部分验收）

新增 opt-in loopback HTTP 与基础管理页，同一 Client/SQL owner 提供状态、来源、
版本化配置/Prompt、工单查看与停止/追加/重试等人工操作。4条新增正式入口测试通过，
当前完整正式入口51条；另在独立空状态以现有授权 GitHub 账号做只读 API/浏览器验证。
准确代码、完整成功/失败日志、源码指纹与边界见
[管理接线](superagent/business-management.md) 和 `docs/business-management-verification.json`。

只将 API-01、UI-01、WORK-01 的已执行子集记录为 in_progress，不签收整个大组。
23组目前5 passed / 14 in_progress / 4 not_run，不是完成功能比例。
账号热切明确拒绝；完整原版查询视图/路由/面板仍未完成。Worker代码交付、六实例配置、
冷切去重、完整备份/守护/回退与生产切换继续未完成。飞书不再要求用户重复手测，
但不会把接入冒烟虚记为真实 Worker/回复闭环。没有重启取消的全域语言兼容工作。

## 2026-11-02：真实 Worker 仓库交付与 PR 报告接线

默认关闭的产品级 `repositoryDelivery` 已提供受限文本快照发布、当前工单/
assignment独立分支、Draft PR及检查/评审读取，不向通用bash提供网络或凭证。
主14/Worker6应用工具不变；这是另一个显式启用的编码能力。

真实验收 D1（issue #3 / PR #4）完成文件、实际 Python测试、远端发布和回执，
但揭示 `pr_number` 报告仍是占位错误，SQL没有绑定PR，保留为部分成功/真实失败。
据此补了 Source实际读取、归属/唯一所有者校验，以及 PR claim/report/relay 同一
SQL事务；分支末段也对齐工作区短名称，不通过放宽归属规则强行接纳旧 PR。

D2 在同一工单新 assignment 上实际编辑、测试、发布 Draft PR #5，Source校验报告
成功，SQL保留 `own=true` 的开放PR，实际回复和重启去重均已核实。
开放PR仍等待后续处理，不把ready称作合并、CI成功或外部评审通过。

本批 55条正式入口测试、选定三crate及17条process回归为387个报告通过，
2条默认忽略的live测试不计通过；17条Node guard通过。不是全workspace测试宣称。
六个旧实例仅做只读在线SQLite快照及副本完整性/九表摘要校验，不是完整回退。
详见 `docs/business-repository-delivery-verification.json` 与
[使用和边界](superagent/business-repository-delivery.md)。完整checkout/依赖构建/SSH、
管理矩阵、冷切游标回执边界、完整快照/回退/首次切换、真实飞书Worker回复及剩余
生命周期分支继续保留；没有切换六个生产实例，没有重启取消的JS全语言迁移。

实现提交 `df6f44f` 的 GitHub Ubuntu/macOS CI现均成功，包含workspace check、
严格workspace Clippy及all-targets测试。后续文档head另验，不把CI成功等同于
完整产品/现网验收；详见 `docs/evidence/worker-delivery-01/ci-implementation-success.json`。
