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
