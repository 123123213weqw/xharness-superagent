# 开发状态

2026-11-02。当前已有正式入口及第一条 GitHub 离线协议整链路，仍未完成全部业务对齐。

最新路径和边界见 [正式入口](docs/superagent/business-entry.md)，当前验收以
`conformance/business/acceptance.json` 为准。以下逐批记录均是当时的范围/统计，
不要把历史“尚无入口”和“23组未验收”当作当前状态，也不要累加重复测试。

- 产品业务核心、SQLite、持久队列/Outbox 和若干原生会话组件已实现。
- 完整工具、Assessor/Executor/Root、Source、API/UI 和部署闭环仍未完成。
- 取消全域 JavaScript 语言语义复刻；不纳入 canonical 的全链路 UTF-16 类型迁移。
- 撤回错误 SDK 工具失败判断，移除据此新增的工具顺序模式。
- 旧 96 条严格 Rubric 均未验收；取消范围不会记成通过。

## 本快照远程验证

- WZU_Server 独立目录：格式检查、全工作区 check、严格 Clippy 均通过。
- SuperAgent 两个产品 crate 及 Core/Tools：39 个 Cargo target，470 项通过，0 失败、0 忽略。
- 固定参考 216 文件及 11 步/86 路径 Runtime 溯源检查通过。
- 本次没有执行完整 workspace 测试；GitHub CI 会执行，不预先宣称通过。
- 不代表完整产品验收。详见 [验证记录](docs/verification-2026-11-01.json)。

## 业务 Goal / B1 进行中

新增 Application 库级装配：状态目录独占锁、SQL/Driver/Passes、四类持久化派发、
Outbox 与统一关闭。5 个新增组合测试及产品模块回归、workspace check、产品模块
严格 Clippy 已在独立远程目录通过。

该 Application 组合记录的外部服务为显式端口；后续 Native 进展见下一节。
该组合本身不能当作完整启动入口或端到端产品通过。
新 23 个业务场景仍未验收，详见 [组合验证](docs/business-application-verification.json)。

## Native 产品调用路径 / B1 继续

已接到实际 Native 主会话/Worker、PromptRuntime、角色绑定和四个业务工具子集，
不再只用记录式 launch 端口。离线组合测试经过真实业务处理器和文件工具，覆盖
执行后回复、拒绝/忽略、批次部分拒绝、模型失败可见、取消关闭、正常重启去重。

正式 CLI、GitHub/飞书 Source、其余工具/资源读取/推断研判、完整生命周期恢复、
配置 API/UI 和部署仍未完成；23 个新业务场景仍为 `not_run`，不把7个离线组合检查
计成完整业务签收。参见 [接线说明](docs/superagent/business-native.md) 与
[本批验证](docs/business-native-verification.json)。

本批在 WZU_Server 独立目录通过 workspace check/严格 Clippy/格式检查；
SuperAgent 两 crate、Host、Agent 回归共862通过、0失败、8个已有忽略项（不计通过）。
不是完整 workspace 测试。固定参考216文件未变；原11步/86路径溯源链不变，
额外17行 Host 观察接口以独立精确 overlay 校验，4个负例检查通过。

## GitHub Source / B1 继续

GitHub 的账号/仓库只读检查、受限 CLI 协议、事件规范化、通知/仓库/订阅线程轮询、
PR/CI 观察和 marker 幂等回复已接入同一产品库。Source → 策略/SQL/队列 → Native
Worker 实际写文件 → 报告/回复的协议夹具场景已执行，不再由调用方手填 Trigger。

这仍没有经过正式产品入口或真实 GitHub 服务；CLI、账号配置与模型的外部边界使用
明确的离线夹具。其余工具、资源/推断研判、飞书、多来源、API/UI 和完整恢复/部署
仍未完成；全产品23组继续 `not_run`。见 [Source 说明](docs/superagent/business-github.md)
和 [本批证据](docs/business-github-verification.json)。正式入口是下一优先项。

本批在独立远程目录通过 workspace check/严格 Clippy/格式检查；5个选定 crate 的
回归906通过、0失败、8个已有忽略项（不计通过），不是整个 workspace 测试。
新增14个 Source/协议/组合检查及57个原版函数样本；当前主/Worker应用工具仍是4个子集。

## 正式入口 / B1 第一条闭环已执行

已有独立 `xharness-superagent` 产品二进制：显式配置 → 实际 gh 子进程 Source →
策略/SQL/Native → 实际 CodingToolBundle write → report/reply/marker Outbox。
外部模型与 GitHub 服务使用明确离线协议夹具，未调用付费模型或在线发评论。
8个正式入口测试通过；新增入口不内置假模型/假文件写入处理器。

本批业务记录5组通过（仅所列离线协议环境）、1组授权场景部分完成、17组未运行，
不是5/23的功能完成率或真实服务验收。当前主/Worker应用工具仍只有4个子集。
参考期望、实际断言与完整日志见 [本批证据](docs/business-entry-verification.json)，
配置/权限和缺口见 [入口说明](docs/superagent/business-entry.md)。

WZU_Server 独立目录：workspace fmt/check/严格 Clippy 通过；9个选定 crate 回归
1286通过、0失败、17个已有忽略（不计通过，含真实/付费模型及诊断项），不是整个
workspace 测试。新代码的跨平台CI另以其精确提交检查，不复用历史绿色状态。

下一优先项：主14/Worker6余下业务工具、bounded read/context 和完整 inferred 研判，
然后飞书/多来源、配置 API/UI、完整停止/追加/重试/后台/崩溃恢复与部署/回退。
这些都留在活动 Goal 内，不把第一条闭环误记成全部业务对齐。

## B2 控制工具接线（当前）

新增7个实际控制处理器，现有主角色6个、Worker5个应用工具；完整20个工具仍缺9个
注册及相关完整分支。正式入口测试现为11个，本批新增线程控制、失败后的新 assignment
重试、普通无报告回合的 waiting/状态查询3条路径；3个控制组件检查不另算业务验收。

修正产品自有 prompt resolver 的关闭竞争；保留原有 Cancelled 断言及真实错误，
定点重复12次通过，未修改冻结的 Host 导入或参考断言。新 CI 须按精确提交复验。

本批 workspace fmt/check/严格 Clippy 通过，但**全工作区测试未通过**：旧
`xharness-feishu --test sandbox` 有2项部署检查失败（专用 UID、出站过滤）。
没有据此改服务器用户/防火墙/代理或现有服务，也没有删测或把它们记成通过。
完整输出、失败范围及其他证据见 `docs/business-controls-verification.json`。

完整 bounded read、note/推断研判/设置/Prompt工具、飞书新 Source、API/UI、完整
后台任务/恢复与部署继续留在 Goal 中。普通安静回合的 waiting 修复不代表原版30分钟
后台/子任务结算已完成。当前仍为5组离线业务通过；新增控制证据只推进相关未完成组。

## 未完成工单恢复接线（当前，部分）

已修复正式入口反例：waiting 工单正常重启后有新评论时，重建原 Native 会话的可信
绑定，继续原 assignment，而非永远留下派发。真正中断的 Worker 用既有 Engine 生成
恢复 assignment；丢失工作目录时不偷偷 prepare/建空目录，保留输入与派发错误。

新增3条正式入口路径：等待/新输入/重放去重、实际 SIGKILL 后恢复且保留部分文件、
目录缺失的负向结果。现14条入口测试通过，外部服务仍为明确离线 HTTP/SSE/gh 夹具。
9个选定 crate 回归、workspace fmt/check/严格 Clippy 通过，不是全 workspace 测试。

前一控制提交 b264618 的 CI 两平台未通过：macOS HTTP 夹具 WouldBlock（本批明确
设置 accepted socket 阻塞模式，待新 head CI 验证），Linux 冻结 Host 的 flush 观察
断言竞争（未改断言或导入）。此前完整 workspace 的飞书 UID/出站过滤失败仍未修复。
没有改服务器权限/服务，也不把范围更小的绿替代上述失败。详见
`docs/business-recovery-verification.json` 和 [恢复边界](docs/superagent/business-recovery.md)。

工具数量仍主6/Worker5；完整 bounded read、其余工具、飞书、API/UI、完整后台/停止/
全部恢复切点及部署回退仍未完成。业务清单现5组离线通过、6组部分推进、12组未运行，
不代表功能完成率。Goal 继续活动，仅对齐产品逻辑，不恢复全域 JS/UTF-16 方向。

## B2 资源读取组件（当前，尚未注册完整工具）

业务读取的 SQL 权限/脱敏/分页层已有7个组件检查；Native 已有会话的被动读取有2个
检查，冷启动可读排队输入、不创建或唤醒 Worker、不重建丢失工作目录。它们不增加
整产品验收通过数，也未向模型注册未接完的 read 工具。主6/Worker5、14条已有正式
入口测试以及业务清单5通过/6部分推进/12未运行均保持不变。

完整 transcript/input/Source/settings/prompts/models 端口、会话快照缓存和正式工具
接线仍是下一步；不能把内部 Plan 当成工具的成功响应或完整 readResource 等价。
见 [边界](docs/superagent/business-resources.md) 和 `docs/business-resources-verification.json`。

本批 workspace fmt/check/严格 Clippy、2个选定 crate 全目标回归通过，不是全工作区
测试。前一35a34a2 CI：Linux通过，macOS旧 gh-host 只等模型 done、未等评论/标签
写入就断言。已补实际 HTTP endpoint 的写入等待，原14个断言逐字保留；新 head CI
仍须另验。冻结 Host 的历史 flush 竞争与 WZU 飞书两项部署失败未据此清除。

完整业务工具、推断研判、飞书/多来源、配置 API/UI、后台/停止/完整恢复与部署回退
继续在活动 Goal 内。没有全局语言类型迁移、新依赖、付费调用或线上服务变更。


## B2 模型侧资源读取（最新，仍非完整业务对齐）

主/Worker 的 `orchestrator_read` 已接同一 SQL/Native/Source owner，不再返回内部计划。
主7/Worker6应用工具已注册；Worker名字齐全不代表 PR/飞书/后台等分支已经完成。
长消息补读、会话隔离快照、实际模型/设置/Prompt、公开会话历史（保留工具调用，
移除推理/私有provider数据）以及读取中 CLI 关闭回收已通过正式入口离线协议路径。

16条正式入口测试通过，本批新增2条；19个原 readGithub+Commands 样本、5个原
独立 transcriptEntry 投影及4个renderer/cache组件检查不单算整产品通过。
明确适配 Zod 的 offset=0 输入默认，未修改通用工具校验器或固定参考文件。
最终源码 workspace fmt/check/严格Clippy、10个选定crate回归通过，不是全工作区；
忽略/能力早退和历史语言回归不算新的业务验收。前一7e5b60a CI双平台通过，
不能拿来给新head背书，也未消除WZU旧飞书部署前置失败。

业务清单仍5组离线通过，7组部分推进、11组未运行；不是功能完成率。下一步是
7个主角色工具、完整推断/来源/配置API/UI，以及后台/停止/恢复和部署回退。
Goal继续活动；没有全域JS/UTF-16迁移、新依赖、付费调用或线上服务变更。
详见[读取边界](docs/superagent/business-read.md)与 `docs/business-read-verification.json`。


## B2 业务配置与提示词（最新，完整对齐仍未完成）

settings_save/prompts_save 已接同一 Client/SQL/Native owner；主9/Worker6应用工具，
尚缺主5注册及各工具完整业务分支。块版本、冲突/no-op、panel-only环境/账号guard、
实际模型选择、Prompt预览/版本/next-request和团队技能加载已走正式工具路径。
正式入口过去未安装组件中的 Skill Manager；本批接入真实内置包、可发现目录及
scoped skill 工具，不把内部注册成功冒充模型可用。全局权限和默认工具未放宽。

新增4条实际入口场景，现20条通过：配置、Prompt/技能、暂停/恢复、SQL策略保存
切点回滚；2条资源安装组件检查不算部署签收。暂停能记录决定，但挂起Worker和
回复，恢复后才真正执行；不是错误地禁止整个主会话决策。

最后一次 workspace fmt/check/严格Clippy、10个选定crate回归通过，不是全工作区。
保留真实反例：新模型默认逻辑一度把 null selector 的 Worker 走到主默认；现已
保存原始分角色默认，旧 product_native 断言未改。前一53fb02b CI两平台通过，
不能代替新head，也未消除WZU旧飞书部署前置问题。

当前业务清单5组离线通过、9组部分推进、9组未运行，不是功能完成率。
Note/三个eval/policy_sync、完整来源、API/UI、后台/停止/恢复/部署继续在活动Goal。
无新依赖、付费调用、真实GitHub评论/生产部署或JS/UTF-16全域迁移。
见[配置边界](docs/superagent/business-configuration.md)与
`docs/business-configuration-verification.json`。


## 2026-11-02 主动研判业务子集

新增 eval/eval_read/eval_decide 实际主角色路径；目前主12/Worker6应用工具，
不是全部分支完成。推断 auto 经当前规则/方向/意图授权后，实际 Worker 写文件并
经 Outbox 回复；observe 只记录建议，不开 Worker。显式请求仍必须 route。

本次正式入口27条通过（新增7条），覆盖引用/路径拒绝、规则/证据/中文确认语
边界、已结束评估、真实双消息批次的当前 trigger 身份、超时及 SIGINT 子进程回收。
另有20个原版 GitHub 上下文/仓库/窗口执行样本和10个原版常见词数样本；
这些组件样本不冒充整产品验收。所有 Rust 验证在 WZU_Server 独立目录执行；
workspace fmt/check/严格 Clippy 与所列10个 crate 回归通过，来源指纹730文件本地/远端一致。

新加 ICU4X 中文/英文确认语分词依赖，6个新增锁定外部包、7个既有区段依赖边；
既有版本/checksum不刷新，保留许可证与独立严格 lock overlay。没有 Node/SDK
生产依赖、全域 JS/UTF-16 迁移、付费或真实外部写入。

上一精确提交 CI 的 macOS 成功，Linux 因 PID 夹具非原子发布读取到空值失败；
现改原子发布并保留所有存活/取消/回收断言。新提交 CI 不能套用旧提交结果。
完整证据见 docs/business-assessment-verification.json 与
docs/superagent/business-assessment.md；仍为5组离线通过、9组部分推进、9组未运行。
主工具还缺 ticket_note/policy_sync，完整来源/飞书/API/UI、后台/停止/恢复/部署
回退继续在同一个活动 Goal 内，未标记完成。
