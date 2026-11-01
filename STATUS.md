# 开发状态

2026-11-01。当前是组件开发快照，没有完整产品启动入口。

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
