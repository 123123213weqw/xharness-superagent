# SuperAgent 行为对齐执行进度

> **历史记录，不是当前 Goal 或验收口径。** 以下保留旧语言级对齐阶段的原始记录；
> 用户已取消全域 JS/UTF-16 等非产品前置要求。当前只执行
> [业务 Goal](../business-alignment-goal.md) 与
> [新业务验收清单](../../conformance/business/acceptance.json)。
> 本文旧“Goal ACTIVE / 40项 / 96条不变”等表述只描述当时状态，不再适用。

完整 Goal **ACTIVE**，M0–M7、40 项任务和 96 条最终 Rubric 不变；最终验收 **0/96**。

## 当前新增：可信装饰握手与时序对齐

Main ensure／Worker 绑定在原本本地 SQLite owner 确认装饰，尚不写 Session revision KV、不唤醒和调用模型。绑定后首次请求前删除 Ticket，仍保留已注册的 Worker 提示，按原版使用默认环境且不附 Mandate。最初没有 Ticket 的会话保持通用行为及当次动态 Host 基础提示，直到显式重新装饰；请求不会擅自识别后来出现的 Ticket。

被动 watch 通道身份区分 actor 更换与 activity 计数／worker 重启。关闭、取消、回复丢失和原始错误所有权有定向验证，但完整 epoch／关闭 ABA 和 skills 服务重启尚未验收。

新增2项真实 Native 集成和5项私有 owner／epoch 声明；产品 owned 共189个唯一声明通过。4个独立原版差分声明比较9次实际离线 Provider 输入和物理 Harness 快照，其中4次走原版 Sessions.decorate 时序。Cordis／技能注册及非相关工具适配器是明确的测试替身，不等于真实技能服务。普通 JSONL 仍省略正文，模型路由、WorkspaceWrite、旧构造器与生产入口未改。

全仓格式／检查／严格 Clippy 通过；两个既有飞书环境测试仍失败，不算全绿。原 Prompt22、Store33、Delivery39、RetryHealth／Models77、legacy5／BasicHost24回归通过。本地 canonical 固定快照未改，受控同步仍6次／48路径；共享 canonical 远端目录发现漂移，保留未覆盖，不算本次远端验收通过；Rust 只在 WZU_Server 编译。

## 下一步（未豁免）

真实 skills scope／工具／持久模型目录、revision 更新、服务重建、resourceBase 和无损 UTF16；真实 AGENTS、SourceRegistry、Assessor／Executor／Root／20工具守卫／API／面板／部署恢复；传输重试与内部摘要、完整 writer 生命周期／同步 facade／增量事件及既有迁移／时区／JSON／控制／飞书偏差继续执行。

详见 Native Prompt 文档与 conformance/execution-status.json 的 native_decoration_progress。

## Live Skills 与真实模型输出组件（2026-11-01T03:13:11.855888+00:00）

完整 Goal ACTIVE；40 任务／96 最终 Rubric 不变，0/96 签收。真实技能作用域、同 revision 服务重启、4 次 SDK 工具返回／模型渲染逐字差分及冷 JSONL 已通过；193 条自有测试独立清单执行通过。canonical 经明确批准在独立验证目录运行，7 次受控导入／53 路径，默认工具 JSON 和权限不变。全仓 fmt/check/strict Clippy 通过；测试仍有两条既有 Feishu sandbox 环境失败，不计全绿。编译/语义变异及清理共享 target 后的重新验证均保留原始失败与恢复证据；不隐藏失败。实际持久化 catalog、直接调用、完整生命周期/UTF-16、Root/Sources/API/UI/恢复/部署等仍未签收。

## 持久 Step 上下文边界（2026-11-01T03:54:42.015839+00:00）

完整 Goal ACTIVE，40 项任务／96 条最终 Rubric 不变，0/96 签收。新增可信 context/message、来源保留、严格 CAS、取消/错误、JSONL 冷读和 UI 投影；8 次受控导入／67 路径，7 条新通用测试与 193 条产品自有测试通过。严格全仓检查通过，测试仍有两条既有飞书 sandbox 失败。实际 SDK 目录回调的 10 次参考轨迹已执行；这不是 Rust 目录对齐证据。产品目录消费者、StepStart 前的完整准备与候选替换/删除、UTF-16 传输，以及完整 Root/Sources/API/UI/恢复/部署仍未完成。

## StepStart 前准备与原子提交（2026-11-01T04:13:29.073251+00:00）

完整 Goal ACTIVE，40 项任务／96 条最终 Rubric 不变，0/96 签收。可选上下文回调移到 StepStart 前，StepStart 与上下文同一 CAS/修订提交；空贡献、取消/错误、暂停/steer、提示词准备和 fsync 失败均有实际 Core 回归。新增 6 条通用测试、累计 13 条 Step 边界测试及 193 条产品自有测试通过；10 个编译器负例、5 个正对照、6 个语义变异有效。第 9 次受控导入仍为 67 个路径。全仓 fmt/check/严格 Clippy 通过，仍有 2 条既有飞书 sandbox 测试失败，不称全绿。该接口仍仅追加；产品目录消费者、完整候选变换、UTF-16 及原 M0–M7 其余工作必须继续。

## 无损目录决策协议（2026-11-01T04:38:26.036748+00:00）

完整 Goal ACTIVE，40 项任务／96 条最终 Rubric 保持不变，0/96 签收。产品核心新增真实目录候选保留／删除／替换、直接调用与完整 UTF-16 键值协议表示；37 条实际 SDK 决策、346 条 JSON／UTF-16 差分及全部 65,536 单码元往返通过。原版 SDK 证实“首次为空且已有匹配空候选”先保留，再考虑空目录抑制；Rust 已按真实分支顺序实现。新增 15 条测试，当前 208 条产品自有测试通过；15 个编译器负例、5 个正对照、6 个语义变异有效。全仓 fmt/check/严格 Clippy 通过，本轮全仓测试观察到 2 条既有飞书 sandbox 失败及 1 次旧 GitHub review 外发完成时序失败；后者同二进制 5 次复测及 Cargo 单测通过，但原失败不豁免。该协议尚未安装到 Native Loop，Core 候选准入变换和 provider／UI／配置的无损传输仍须完成；不以离线协议差分代替完整产品验收。

## Canonical 候选准入变换（2026-11-01T05:13:05.950084+00:00）

完整 Goal ACTIVE，40 项任务／96 条最终 Rubric 维持 0/96 签收。Canonical 新增显式开启的 durable proposal／decision，在原始输入进入模型历史前完成保留／删除／替换；StepStart 与选定输入同 CAS，Blocked 不伪装完成或网络失败。16 条新增 Rust 测试、15 个真实编译器负例、7 个语义变异、14 条实际 UI 终态组装测试及严格 TS 检查通过。工作区 check、8 个受影响 crate 的严格 Clippy／测试通过，不等于全仓测试。修复无 user/message 时丢失广播的 Host 身份恢复；终态契约显式升级 v2，冻结 v1 不改，新增 blocked 要求先升级读端再开启。Runtime 核心仍未回迁此能力，既有 9 次导入／67 路径保持原样；Native 目录、完整 UTF16 传输、自动重排队／业务 ActorEpoch 恢复和全部原始产品范围仍未签收。既有 Runtime 全仓三项失败仍保留未决。

## Runtime 输入候选变换回迁（2026-11-01T05:41:43.964859+00:00）

完整 Goal ACTIVE，40 项任务／96 条最终 Rubric 仍 0/96 签收。第 10 次受控同步导入 29 路径，累计 86 路径；19 个首入路径绑定双仓固定 HEAD。保留 Runtime 原有 inline Inbox 验证与 observer 控制算法；两处上下文适配经过审查与实际回归，不整文件覆盖。16 条回迁 Kernel 测试、208 条当前产品自有测试、15 个真实编译器负例／7 个语义变异、9 个来源校验负例通过。全仓 check 和严格 Clippy 通过；本轮全仓仅两项既有飞书 sandbox 环境失败，旧 GH 用例本轮通过但其外发完成时序 race 未修复、未豁免。Runtime 实际导出的 v2 schema／八组终态事件与升级读端字节一致，14 条生产 UI 组装测试和严格 TS 通过；冻结 v1 不改，要求先升级读端再启用能力。37 条实际 SDK 目录决策／346 条 JSON-UTF16、4 条旧 Native 技能模型输出差分继续通过。Generic 能力已在 Runtime，但 Native 目录消费者、完整 UTF16 传输、完整 SDK AgentLoop oracle、业务自动重排队／恢复及全部原始产品范围仍未验收。

## 零步 UI 反例与修正（2026-11-01T06:01:31.145815+00:00）

上一节 14 条一般终态用例没有覆盖零步回合，不能据此认定此路径一致。补充实际检查发现 input/proposed 的 data.step 让历史回放凭空建立 Step，而实时流没有。Canonical 已改用 proposedStep（持久 InputProposal.step 不变），第 11 次受控同步到 Runtime，累计仍 86 路径。当前全仓 check／严格 Clippy、10 个受影响 crate 的测试及 17 条 Kernel 回归通过；真实 Rust Session 日志投影进入生产 UI，在首回合 turn=0 的拒绝／空完成／取消三种情况下均无 Step、live/history 相等且没有未发布正文。新增实际语义变异有效，当前来源校验 12 个负例通过。完整全仓测试发生在仅投影修正之前，两项旧飞书失败和旧 GH race 仍未豁免；没有将一般测试或手工数据序列冒充完整 AgentLoop 捕获。原 40 项任务／96 最终 Rubric 保持 0/96，Native 目录、完整 UTF16 传输和原始完整产品范围继续执行。
