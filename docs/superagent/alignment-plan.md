# XHarness SuperAgent 行为对齐执行计划

日期：2026 年 10 月 31 日，北京时间。目标：在 XHarness 上实现与当前开源 SuperAgent 对齐的产品，同时保持 XHarness 现有行为契约不回退。

采用双基线和逐项验收：上游是产品行为参考，XHarness 是运行时非回归参考。主产品使用 Rust；官方 TypeScript 代码作为可执行参考，不移植 Cordis 插件加载器，也不重写已有 Agent Loop。先打通 GitHub 闭环只是里程碑，不是“全面对齐”完成。

本计划定义待执行工作，所有行为验证初始为 not_run，不代表当前产品已经通过。本文与 JSON rubric、执行 backlog 必须一起版本化；改变基线必须走显式差异审查，不能随 main 漂移。

## 目标与一致性定义

“一致”分五层，不以模型最后一段文字是否逐字相同为标准。

1. **领域一致**：同输入、策略、模型决策和时钟下，接单、拒绝、授权、订阅、工单状态、外发意图一致。
2. **执行一致**：相同脚本化模型和工具下，消息投递、工具调用、停止、后台工作、重启恢复的可观察轨迹一致。
3. **模型输入一致**：Prompt、工具 Schema/description/顺序、来源标识、资源指针、截断和上下文呈现没有未经批准的语义变化。
4. **产品一致**：GitHub、飞书、管理 API、配置、面板、部署操作可完成相同工作，不以像素复刻作为目标。
5. **原产品不回退**：既有 XHarness 的日志真源、输入收据、权限、恢复、模型选择、平台工具和旧入口继续遵守原契约。

发布要求所有必需项通过；P0 零失败。P1 也不能因总分高而省略。skip、not_run、未覆盖的平台都不算 pass。批准的偏差只能形成“兼容版本”，不能标成严格全面对齐。

范围以固定提交里的实现、默认 Prompt、工具和测试为准；研究文章未实现的能力、通用个人助理、额外长期记忆、新定时任务产品、UI 重设计不自动纳入。

## 固定代码基线

| 用途 | 仓库 | 固定提交 |
| --- | --- | --- |
| superagent | [deepseek-ai/dsh-superagent-prototype](https://github.com/deepseek-ai/dsh-superagent-prototype/tree/6337fc823ce20251c8de8085948165c81aaa701e) | `6337fc823ce20251c8de8085948165c81aaa701e` |
| runtime | [123123213weqw/xharness-runtime](https://github.com/123123213weqw/xharness-runtime/tree/2147ff270fe55c2c739e5063a5bfacefd01dc38a) | `2147ff270fe55c2c739e5063a5bfacefd01dc38a` |
| xharness | [123123213weqw/x-harness-rs](https://github.com/123123213weqw/x-harness-rs/tree/4ab403e0bb671b69fe56c5355bcf10dad7010c0b) | `4ab403e0bb671b69fe56c5355bcf10dad7010c0b` |

SuperAgent 对应研究原型首个公开提交。开发开始前若选择更新任一基线，必须生成旧新提交差异、rubric 变更、默认值变化和受影响夹具清单，再更新 lock；禁止偷偷使用最新 HEAD。

源码与文档冲突时，由参考实现的实际轨迹和测试共同裁决，并记录文档差异。发现缺陷不能悄悄复制或悄悄修复：保留 reference 行为证据，创建偏差决议并说明产品选择。

参考测试清单共 59 个顶层测试文件，全部已登记到 rubric 分组；这是测试文件数，不是用例数，更不是已通过数。M0 必须继续提取各文件断言和参数化分支，防止仅按文件名宣称全覆盖。

## 现状和必须处理的差异

XHarness 已有持久 Runtime、Followup/Steer/Inject、按会话工具工厂、GitHub 和飞书任务入口；这些作为复用接口，不视为已完成产品兼容验收。[运行时接口](https://github.com/123123213weqw/x-harness-rs/blob/4ab403e0bb671b69fe56c5355bcf10dad7010c0b/crates/xharness-host/src/runtime.rs)，[现有 GitHub 入口](https://github.com/123123213weqw/xharness-runtime/blob/2147ff270fe55c2c739e5063a5bfacefd01dc38a/crates/xharness-gh/src/server.rs)。

| 现状 | 对齐要求 |
| --- | --- |
| GitHub 按 repo/issue、飞书按 chat/workspace 组织会话 | 独立 Thread、Ticket、Assignment；同线程可拥有多个工单 |
| 现有 GitHub 触发偏向 owner 的 `/xh`，PR 自动走 review | 另建 SuperAgent 来源与研判，不改变旧 `/xh` 行为；PR 上也能根据授权处理不同任务 |
| 主要读取最终 Loop 结果并归为 done/stopped | 结构化 progress/waiting/ready/blocked/failed/dropped，工单结束由业务规则决定 |
| GitHub 回写与完成 marker 不是完整持久 Outbox | 外发意图、顺序、收据、重试和不确定结果对账；不能用单评论槽覆盖多条沟通 |
| 工具工厂主要装 CodingToolBundle 和 gh_comment | Router/Worker 独立工具集、20 个应用工具、Host 二次注入门禁 |
| 原 runtime 与主仓库提交不相同 | 不把主仓库新 API 当作 runtime 已有能力；先做能力清单与受控同步 |

必须在 M0 决议的兼容差异：

- D01：DSH 产品 stop 清理 Inbox，而 XHarness 普通 cancel 保留待处理输入。产品级 stop 必须独立实现，不改变其他会话 cancel 的默认语义。
- D02：DSH Worker 可使用的子 Agent/嵌套能力与 XHarness 原有委派限制不同。逐项验证装配和能力，所需扩展通过显式产品策略提供；不放宽旧父子权限。
- D03：子模型选择与角色模型配置不能绕过 XHarness 现有人类授权。操作者静态配置与模型临时请求必须区分。
- D04：参考 Worker 默认 full-access 不意味着可以关闭既有沙箱、凭据隔离或 Rust 远程编译规则。部署策略优先，差异明确登记。
- D05：参考默认会话、命令工具、上下文压缩和源码指令可能与 XHarness 默认值不同，必须用模型请求轨迹证明兼容，而不是只保持工具同名。

这些差异在测试和配置中明确解决前，严格对齐结论保持未完成。

## 目标架构和代码所有权

```text
GitHub Source / Feishu Source
             |
      Trigger / Thread
             |
    Gate / Eval / Policy
             |
 Ticket / Mandate / Assignment ---- SQLite 业务库和 Effects
             |                              |
   XHarness Runtime Adapter                 Outbox
             |                              |
 Durable Agent / JSONL / Tools / Jobs ---- 原讨论和管理面板
```

建议在 xharness-runtime 新增自有组件，名称为拟定名称，不表示当前已经存在：

- `xharness-superagent-core`：纯领域模型、准入、工单状态机、端口与不变量，不依赖 Host、UI、网络和真实时间。
- `xharness-superagent`：SQLite、Effects、Source、Outbox、Prompt、配置、Runtime 适配、API 和独立服务组合；模块按职责拆开。
- `conformance`：官方 TypeScript 参考适配器、脚本化 Provider、来源客户端、轨迹比较器和用例数据。

优先把产品代码留在 runtime 的自有范围；同步过来的 Rust 核心只在确有缺口时通过 x-harness-rs 单独变更，再同步到 runtime。避免 Host→产品→Host 循环依赖；顶层组合根同时装配产品与运行时。

当前同步脚本的 OWNED_CRATES 仅包含 xharness-gh 和 xharness-feishu，新增自有 crate 必须同步更新，否则脚本会把它当作上游闭包并报不存在。该脚本末尾执行 cargo check，**禁止在本机运行**。[同步脚本](https://github.com/123123213weqw/xharness-runtime/blob/2147ff270fe55c2c739e5063a5bfacefd01dc38a/scripts/sync-upstream.sh)

参考核心本身无 I/O，除 Node 哈希外不依赖域外模块，适合直接运行作 oracle；外部行为不能靠重写文档示例来伪造参考结果。[参考架构](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/6337fc823ce20251c8de8085948165c81aaa701e/docs/007-architecture.md)

### 数据和运行时端口

业务 SQLite 是 Trigger、Thread、Decision、Mandate、Ticket、Assignment、Subscription、Effects、Outbox、版本配置和执行映射的真源；会话历史和 Inbox 只以 XHarness JSONL 为真源。业务库可保存引用和结算收据，不复制完整会话再派生一套历史。

Runtime 端口至少覆盖 ensure/restore、followup、steer、inject、输入收据、事件订阅、cancel、jobs/subagents、角色模型、Prompt 和工具装配。复用 DurableAgentHandle、AgentRuntime、SessionToolFactory；不用 start_turn 假装全部三种投递。

角色注册必须覆盖工具工厂与 Host 自带工具两个入口；应用工具依原 Schema 暴露，身份、权限和工单由宿主绑定。模型请求前一次性确定正确角色、Prompt revision、Mandate 和工具快照。

业务执行采用持久 intent→执行→receipt 和对账，而不是双数据库分布式事务。可确认未执行的工作可以安全重试；未知副作用先核实。不得因“Rust 端报超时”就重新执行一次工具或重新发 PR。

## 参考默认值和关键时间契约

以下是参考代码的默认值，执行时导出成机器快照并校验；不是可以随意调整的性能建议。

| 项目 | 参考值或语义 |
| --- | --- |
| 主动模式 | observe，默认 copy 与 visual 两条规则保持原文 |
| 研判超时 | 120 秒，从候选真正送到主会话后计时；暂停期间顺延 |
| 普通并发 | 3，另一个槽用于人类消息或状态请求 |
| GitHub 轮询 | 默认 60 秒，更长服务端要求优先；observe 全量接收开关默认关闭 |
| prepare | 首次目录不存在时执行，最长 10 分钟 |
| Router/Worker 前台命令与 job wait | 5 秒后交还，长命令变后台；Router 另有不等待约束 |
| Worker 后台 Turn 宽限 | 最长 30 分钟；人工交接后保留的评审作业按独立规则处理 |
| 模型暂态故障 | 有界三次重试，退避从 5 秒增长至上限 300 秒；非暂态与 prepare 失败另处置 |
| 输入和 relay 等投递 | 从 2 秒退避到上限 60 秒；不同 effect 的并发与顺序分别对齐 |
| 外发消息 | 从 5 秒退避到上限 300 秒；同工单同目标保序，前序连续失败三次后的放行例外也须匹配 |
| 上下文读取 | 默认 10 页、单次读取 15000 毫秒；Prompt 总字符预算默认 100000 |
| 自动合并 | 默认由人合并；只有显式配置时采用参考条件，不能以“PR 创建”作为 ready |

最小模型工具清单必须完整：Router 的 orchestrator_read、orchestrator_route、orchestrator_ticket_retry、orchestrator_ticket_request_status、orchestrator_ticket_note、orchestrator_trigger_requeue、orchestrator_eval、orchestrator_eval_read、orchestrator_eval_decide、orchestrator_reply、orchestrator_thread_unsubscribe、orchestrator_settings_save、orchestrator_policy_sync、orchestrator_prompts_save；Worker 的 orchestrator_read、orchestrator_ack、orchestrator_report、orchestrator_thread_unrelated、orchestrator_thread_follow、orchestrator_trigger_handback。同名 read 的权限按角色分开。

## 行为 Rubric

共 96 条验收要求，每条可派生多个参数化测试，不等于 96 个测试用例。所有条目初始 not_run，机器版包含固定来源、参考测试分组、计划夹具路径和空证据栏。

P0 是授权、数据、副作用或生命周期硬门槛；P1 是完整产品必须对齐的行为。全面对齐要求两类均通过，不提供掩盖关键失败的加权总分。

### 事件来源

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| SRC-01 | P0 | **输入事件持久化与去重**：相同 Source 事件重复送达只形成一个 Trigger；Source 游标不得先于事件落盘推进。 |
| SRC-02 | P1 | **GitHub 接收范围**：对齐 notifications、订阅 Issue/PR、CI、关闭合并及四个 observe 开关；编辑旧正文增加 mention 不冒充新通知。 |
| SRC-03 | P0 | **忽略自身与范围外事件**：自身机器人消息和未监听会话在进入接单域前过滤，保留可解释的 disposition。 |
| SRC-04 | P0 | **订阅扇出与上下文**：已订阅线程的消息送达全部相关工单；context-only 订阅不授权、不恢复已结束工单。 |
| SRC-05 | P1 | **飞书接收与历史恢复**：配置群、其他群、私信、mentions/all、历史补读和断线恢复与参考一致。 |
| SRC-06 | P1 | **来源上下文分页和图片**：原消息、根消息、回复关系、分页游标、图片引用和有界读取保持可追溯，不伪造完整内容。 |
| SRC-07 | P0 | **来源身份隔离**：GitHub login 和飞书 open_id 分别判断；跨来源身份说明只作为知识，不自动合并授权。 |
| SRC-08 | P1 | **来源故障与健康状态**：按参考的逐线程失败、退避、连续失败计数和不可达会话处理；短暂错误不能直接判定永久失效。 |

### 接单与授权

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| AUTH-01 | P0 | **明确请求路径**：允许名单中的人明确提及或私信 Agent 时走 explicit 路径；该路径在 off、observe、auto 均可工作。 |
| AUTH-02 | P0 | **准入表组合**：逐格覆盖 listed/unlisted、addressed/unaddressed、接收范围和 explicit/inferred/ignore；保留显式准入例外。 |
| AUTH-03 | P0 | **机器人不能形成明确授权**：只有配置 bot list 的机器人可形成 inferred 候选；机器人不能明确派活或停止工单。 |
| AUTH-04 | P0 | **off 模式**：拒绝所有 inferred 新候选，不影响合法 explicit 请求；不能把 off 误实现为全局暂停。 |
| AUTH-05 | P0 | **observe 模式**：正向研判只记录 would_accept 和提案，不创建工单、不启动 Worker、不发送接单确认。 |
| AUTH-06 | P0 | **auto 模式和逐规则研判**：主动接单至少命中一条启用规则；每条启用规则恰评一次，重复、遗漏和过期研判不得接单。 |
| AUTH-07 | P0 | **策略版本变化**：过期 Eval 丢弃并重新排队；只给受影响工单发策略通知，不直接粗暴取消全部运行工单。 |
| AUTH-08 | P0 | **Mandate 作用域**：消息、引用、模型输出、跨工单 note 和订阅都不能扩大 Mandate；扩大任务范围需新授权工单。 |
| AUTH-09 | P0 | **恢复时重新校验**：已结束工单恢复前检查当前来源范围与 Mandate；规则或身份不再有效时记录撤销，不偷偷继续。 |
| AUTH-10 | P0 | **默认规则与默认值**：默认 copy、visual 两条规则及原文、observe、120 秒研判超时、3 个 Worker 与空名单默认准入行为保持一致。 |

### 工单领域状态

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| DOM-01 | P0 | **线程内研判串行**：同 Thread 最多一个 Eval；处理中到达的事件进入下一批，不并发作出互相覆盖的判断。 |
| DOM-02 | P0 | **同线程多个工单**：Thread 与 Ticket 多对多，不能用 repo+issue 或 chat_id 直接替代 Ticket 身份。 |
| DOM-03 | P0 | **接单原子性**：Decision、Mandate、Ticket、Assignment、初始订阅和接单消息意图原子提交。 |
| DOM-04 | P0 | **派工独立生命周期**：Assignment 的 pending/running/ended 与 Ticket 的 open/ended 独立；重试保留工单与工作成果。 |
| DOM-05 | P0 | **结束后人类续办**：满足 Mandate 的原任务可由所属线程的人类跟进恢复；机器人和 CI 不唤醒 ended 工单。 |
| DOM-06 | P0 | **人工停止不可自动恢复**：stop 撤回 Mandate、终止自动输入；后续普通消息、CI、重启均不能偷偷恢复旧工单。 |
| DOM-07 | P0 | **跟进所有权和退回**：Worker 先决定自己拥有的跟进；全部拥有者 handback 或未读释放后才重入 Eval，Router 不重复接单。 |
| DOM-08 | P0 | **PR 结束条件**：有 PR 的 ready 只表示等待评审；按所拥有/跟随 PR 的关闭合并及尚未处理的跟进决定真正结束。 |
| DOM-09 | P1 | **无 PR 交付和无需处理**：无 PR 的 ready 与 dropped 按参考结束；dropped 默认安静，不能伪装为失败或已实现。 |
| DOM-10 | P0 | **独立会话和工作目录**：每 Ticket 一会话一目录；prepare 首次执行、10 分钟上限、失败与重试、分支/PR 归属规则一致。 |

### 运行时适配

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| RUN-01 | P0 | **三种消息语义**：queue/followup、steer、inject 分别对齐后续 Turn、Step 输入、仅上下文不唤醒；不能统一成 start_turn。 |
| RUN-02 | P0 | **持久准入收据**：稳定 message_id 在准入 Flush 后才确认；区分已排队、已领取、已结算，重试不重复送给模型。 |
| RUN-03 | P0 | **先订阅再唤醒**：新会话与恢复会话先挂接事件观察者再唤醒，避免遗漏开始/结束事件。 |
| RUN-04 | P0 | **按输入归属观察结果**：结果绑定 input/turn/assignment，不以整个 Agent 的 idle 或上一轮缓存推测本次请求完成。 |
| RUN-05 | P1 | **角色模型选择**：Router 与 Worker 使用各自配置；已有会话不被新工单默认模型设置悄悄覆盖，显式用户选择继续有效。 |
| RUN-06 | P0 | **每次请求前装配上下文**：模型请求前安装正确角色、当次 Prompt revision、Mandate、AGENTS 与工具集，不混入另一角色默认人格。 |
| RUN-07 | P0 | **并发和人类优先槽位**：默认 3 个普通 Worker 槽，加一个供人类消息/状态请求使用的槽；prepare 和后台工作占槽语义一致。 |
| RUN-08 | P0 | **后台工作不等于空闲**：Turn 结束仍有 jobs/subagents 时继续 working；结果自动送回，不能提前等待或释放槽位。 |
| RUN-09 | P0 | **交接和后台寿命**：交人工时子 Agent 停止，供评审的 shell 作业按参考保留；Assignment 结束统一回收，不影响别的工单。 |
| RUN-10 | P0 | **停止与清队列语义**：产品 stop、Assignment 取消、人工暂停、宿主关闭分别实现；显式处理 DSH 清 Inbox 与 XHarness 默认保留输入的差异。 |

### 模型工具协议

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| TOOL-01 | P0 | **Router 的十四个应用工具**：工具名称、JSON Schema、description、结果形状和批量路由的逐项拒绝行为逐项对齐。 |
| TOOL-02 | P0 | **Worker 的六个应用工具**：read、ack、report、thread_unrelated、thread_follow、trigger_handback 的授权检查和结果一致。 |
| TOOL-03 | P0 | **基础工具与角色边界**：对齐角色工具白名单；覆盖 Factory 和 Host 二次注册入口；不能用全部 coding-tools 替代受限工具集。 |
| TOOL-04 | P0 | **结束工单的工具门禁**：ended 工单拒绝新工作工具；blocked/failed 工单仍允许人类直接在会话中恢复。 |
| TOOL-05 | P1 | **工具参数和返回契约**：严格校验、长度/大小限制、错误 code/values、图片附件和分页资源指针一致，不靠自由文本猜结构化状态。 |
| TOOL-06 | P0 | **可信身份绑定**：调用者、父子关系、工单、来源身份由 Host 绑定，模型不能通过参数自选权限、路径或授权身份。 |

### 提示词与模型输入

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| PRM-01 | P1 | **提示词顺序**：对齐 shared/role、团队三段、订阅回复、dispatch/work、admission、handoff、environment、policy 顺序和角色差别。 |
| PRM-02 | P1 | **默认文本和语义约束**：固定英文默认文本与工具说明；Routing note、消息字数、review 轮次、真实截图/验证要求不得无意删改。 |
| PRM-03 | P0 | **版本保存与生效边界**：每次保存不可变版本与 CAS；每次模型请求读最新 revision，保存本身不唤醒、不取消、不重启会话。 |
| PRM-04 | P0 | **上下文不成为授权**：外部文本、技能、用户可编辑 prompt 不能修改 Host gate、Mandate 或工具检查。 |
| PRM-05 | P1 | **资源有界读取**：Trigger 窗口、截断、分页、图片、原始来源和 prompt revision 读取权限逐项匹配参考。 |
| PRM-06 | P1 | **覆盖与升级**：覆盖保留 base、模板必需占位符校验、默认变化比较、保留或采用新默认、历史恢复均对齐。 |

### 沟通和交付

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| DEL-01 | P0 | **接单确认恰一次意图**：对每个新请求确认对象与交付物；抑制规则有效；不因重试重复生成确认意图。 |
| DEL-02 | P0 | **回到原讨论**：确认、问题、结果路由到正确原消息/线程；绑定的 reply purpose 不能越权选择目标。 |
| DEL-03 | P1 | **低干扰和结构化问题**：工作过程安静；human_action 一次一个具体问题，Router 原样转达，允许其他已授权工作继续。 |
| DEL-04 | P0 | **持久 Outbox 和消息顺序**：每条外发消息独立身份，同工单同会话保序，并匹配前序连续失败三次后的放行例外；不能用单评论槽覆盖所有沟通。 |
| DEL-05 | P1 | **不可达目标与来源关闭**：来源 off 时等待；会话永久不存在或移出监听范围才 dropped，保留原因和审计。 |
| DEL-06 | P1 | **PR 交付与评审规则**：草稿不报 ready；按参考证据、已验证/未验证、评审解决/拒绝、重复工作退出规则交付。 |
| DEL-07 | P0 | **人工合并与自动合并**：默认人工合并；开启自动合并时保留指定账号、head 审批、少于 200 行、新 revision 撤回等参考条件和现有用户授权。 |

### 故障与恢复

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| REC-01 | P0 | **先记账再副作用**：launch、delivery、assessment、relay、notice、reply 先持久排队；回滚事务不得执行取消等外部副作用。 |
| REC-02 | P0 | **崩溃切点矩阵**：在输入、接单、派工、工具调用/结果、结算、外发、回执等持久切点杀进程，恢复后不丢任务或重放未知副作用。 |
| REC-03 | P0 | **暂态失败分类与退避**：408/429/5xx 与连接类暂态错误按参考有界重试；prepare 失败不冒充可自动重试的模型错误。 |
| REC-04 | P0 | **失败后的自动门禁**：blocked/failed 后只记录自动反馈不唤醒；明确人类输入、合法 retry 或有效后续报告才能解除。 |
| REC-05 | P0 | **重启保持状态**：保留会话、目录、订阅、未读输入和输入消费收据；仅给中断 Assignment 新建恢复派工，不重做已结算工作。 |
| REC-06 | P0 | **全局处理暂停**：暂停调度、Eval 超时和自动投递/回复，来源仍可记账；正在执行的 Turn 不被粗暴取消；恢复时按当前授权处理。 |
| REC-07 | P1 | **来源暂态不可读**：单线程/单 PR 读失败不丢失其他来源；GitHub 404 可能为权限问题，不立即认定永久删除。 |
| REC-08 | P0 | **外发结果不确定**：外发已生效但收据丢失时先回查/对账；不能无条件重发，也不宣称平台间完整 exactly-once。 |
| REC-09 | P0 | **双实例与并发写入**：同 stateDir 单实例锁、同会话 Lease 和事务写入所有权有效；过期代事件不覆盖新派工。 |

### 配置与控制接口

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| CFG-01 | P0 | **本地 API 调用边界**：兼容管理 API 头、Host/Origin 校验、账号检查与权限，不把新管理端点变成未经授权的执行入口。 |
| CFG-02 | P0 | **版本化配置保存**：settings 分块整块值与 revision、prompts baseRevision、400/404/409 等响应语义一致；冲突保留草稿。 |
| CFG-03 | P0 | **配置变更生效与重启**：账号/仓库变更先探测后保存，必要时重启产品服务；模型、并发、pause 等不错误改变 Policy version。 |
| CFG-04 | P1 | **非法配置可诊断**：未知键、模板缺失、无效限制、缺模型/账号使 setup 明确不可用，不截断、不默认假成功。 |
| CFG-05 | P0 | **配置权限分层**：面板能改执行环境和账号；Router 工具不能修改这些部署事实。 |
| CFG-06 | P1 | **允许名单同步**：飞书群成员同步在启动与显式 Sync now 按参考运行，版本变化与运行中策略通知一致。 |
| CFG-07 | P0 | **受控执行环境**：专用凭据、git 身份、wrapper、执行变量与 blocked commands 保留；不把密钥注入模型上下文。 |

### 管理面板

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| UI-01 | P1 | **总览优先人工事项**：先显示待回答/评审/故障，再显示健康和历史；健康空状态不刷屏。 |
| UI-02 | P1 | **六个主标签和过滤**：Overview、Source、Trigger、Eval、Execution、Settings 的数据、计数、链接和状态一致。 |
| UI-03 | P0 | **操作真实生效**：retry、stop、reevaluate、unsubscribe、pause、sync、check again 调用真实控制面，拒绝和确认可见。 |
| UI-04 | P1 | **历史与会话跳转**：能从工单进入对应 Worker，追踪 Trigger→Eval→Ticket→Assignment；搜索 id、链接、文本和线程。 |
| UI-05 | P1 | **草稿和 Prompt 比较**：版本冲突不丢草稿；查看完整 prompt、版本历史、Default updated、恢复默认等入口完整。 |
| UI-06 | P1 | **状态可达与双语**：中英文状态、固定错误说明、键盘和窄屏关键操作可达；不要求像素复刻 DSH UI。 |

### 部署与运维

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| DEP-01 | P0 | **独立服务生命周期**：独立配置与状态目录、端口、单实例、start/status/stop/restart；不停止用户其他会话。 |
| DEP-02 | P0 | **账号和可选来源**：GitHub 专用用户账号不是 App；飞书为可选 bot-only 配置；缺飞书不能拖垮 GitHub。 |
| DEP-03 | P1 | **状态码与修复引导**：对应 0 正常、1 服务故障、2 部分组件故障、3 待设置，界面指向实际可操作修复。 |
| DEP-04 | P0 | **人工停止优先**：自动健康检查尊重手动停止；不会反复重启缺账号/配置的服务；无生产模型不得静默切 mock。 |
| DEP-05 | P0 | **升级和回滚**：备份业务库与会话日志，版本迁移可验证；旧二进制不能强读新 schema，回滚恢复匹配快照。 |

### XHarness 现有行为不回退

| ID | 级别 | 必须满足的行为 |
| --- | --- | --- |
| XH-01 | P0 | **既有入口默认不变**：新产品默认关闭或显式独立入口；原桌面/Web、gh-host/feishu-host 协议和现有会话无静默迁移。 |
| XH-02 | P0 | **JSONL 是会话唯一真源**：新业务库只管产品状态；不得复制聊天历史形成第二会话真源，UI 和结果仍依据持久输入收据。 |
| XH-03 | P0 | **持久准入和原子 Claim**：保留 admit_turn Flush、Inbox Claim 与 turn/start 原子追加、跨列表 message_id 去重和恢复顺序。 |
| XH-04 | P0 | **未知工具副作用不重放**：已写 Tool Call、无 Tool Result 的不确定执行不得自动重跑；审批恢复不伪造工具已执行。 |
| XH-05 | P0 | **可信来源与失败门禁**：内部通知不是人类输入；失败后的自动派发、人工停止和源身份边界不被产品适配绕过。 |
| XH-06 | P0 | **监督关闭和进程回收**：保留 Lease、代际所有权、单一 Supervisor、取消收据、结构化 shutdown 与真实后台任务回收。 |
| XH-07 | P0 | **模型路由和预算**：保持 Chat/Responses 显式协议、有效部署上下文窗口、TokenGuard、压缩和模型选择约束。 |
| XH-08 | P0 | **子 Agent 原有授权**：不放宽父子身份、目录、权限、模型切换人工授权；新产品能力须显式配置且原有语义不变。 |
| XH-09 | P0 | **平台和工具边界**：保留平台沙箱、文件/进程所有权与审批；兼容上游工具名不等于放开全部平台能力。 |
| XH-10 | P0 | **架构和业务错误门禁**：保持 forbid unsafe、recoverable error guards、架构变异测试和真实编译器门禁，不以 grep 代替。 |
| XH-11 | P0 | **Rust 远程编译**：全部 Rust 构建、检查、测试、Clippy 先同步未提交源码后在 WZU_Server 或已配置远程 CI 执行，绝不本机回退。 |
| XH-12 | P0 | **原测试和原生平台覆盖**：受影响原测试、全工作区、进程单线程回归及所声明平台原生远程 CI 继续过门；跳过不计通过。 |

## 接口与状态快照清单

M0 必须导出真实 Schema 和默认值快照，字段缺失、null、默认补全、拒绝、错误码均测试，不能只列函数签名。稳定管理契约至少包含以下项目。

| 管理接口 | 必须保留的可观察结果 |
| --- | --- |
| GET state | setup、accounts、health、load、paused、masterId 及对应状态 |
| GET settings | 分块 value/revision、来源 kind/installed/form/setup、policyVersion、执行环境 |
| GET sources | 来源状态、health、setup、24 小时统计、filtered 和 dropped 原因 |
| PUT settings/<block> | 整块保存和默认补全、revision、policy_changed、restarting；陈旧版本为 409 |
| POST executor/pause | 独立处理暂停，不混同 off 模式或单 Agent pause |
| POST setup/check | 真正触发设置重检；不需要修复时按参考拒绝 |
| POST policy-sync/<source id> | 来源允许名单同步结果和后续 Policy 变化 |
| GET/PUT prompts | current/defaults/sections、保留名、baseRevision、不可变新版本 |
| POST tickets/<id>/retry | 合法状态才创建新 Assignment；保持原 Session、目录、分支和 PR |
| POST tickets/<id>/stop | 撤回授权和停止订阅输入，持久化人工停止语义 |

兼容路径为 /orchestrator/api，保留 x-orchestrator-request、Host/Origin 校验。参数拒绝 400、缺失 404、版本冲突 409、未授权来源 403 等按实际参考分类。面板使用的其他私有路由也要列入快照，但不无意承诺永久公共协议。

领域快照覆盖 Decision 的 accept/stop/ignore/unavailable、Strength 的 explicit/inferred、Assignment 的 pending/running/ended、Report 的 progress/waiting/ready/blocked/failed/dropped，以及 waiting-for-person、queued、working、idle 等派生 stage。保留 outcome 与主体，不能压成一个 done 布尔值。

每个工具保存输入 JSON Schema、description、输出结构和错误结构；批量路由 1 至 20 个独立决定，单项拒绝不回滚其他合法项。Prompt 快照包含原文、组装顺序、默认技能、保留占位符和每次请求的实际 revision。

## 测试设计和证据规则

### 确定性双引擎差分

同一个 scenario 驱动官方参考和 Rust 产品，输入包含操作序列、Source 响应、模型脚本、工具结果、时钟推进与故障切点；记录可验证的 canonical trace。

比较接单/拒绝、Policy version、Mandate scope、工单与派工、订阅所有权、输入队列/领取/结算、Provider 请求、工具调用、Outbox、状态和外部可见结果。并发比较必要的因果偏序及每个 Channel 的顺序，不把独立任务调度先后误当差异。

规范化只允许一一映射的非业务 ID 和绝对时间基点；保留 ID 关联、时间差、重试次数、来源身份、权限、错误码、消息次数、字段缺失与 null 的差别。模型工具声明顺序、Prompt 原文和分页窗口不能被宽松比较器抹掉。

每条夹具至少有 reference trace、target trace、结构化 diff、实际外部客户端调用记录。比较器需接受故意注入的错误实现，证明错误接单、重复执行、无权恢复、丢输入和错误回写都会失败。

### 参数化和边界

准入至少覆盖：来源×接收范围×发送者类型×名单×是否提及×准入格×主动模式×规则命中×策略版本。生命周期覆盖：工单 open/ended/stopped、输入 own/context、Worker running/idle/failed、PR open/merged/closed、人工/机器人/CI。

字符和数量限制从参考 Schema 导出，测试边界前一值、边界值、后一值、空值、未知字段和重复项。不能从最终输出猜测结构化条件，也不能只验证 happy path。

### 故障切点

必须包括：Source 事件落盘前后、游标推进前后、Eval 投递与超时、接单事务提交、prepare、输入准入 Flush、Claim、Tool Call Flush、Tool Result Flush、Turn End、Outbox 入队、外部已发送但收据未写、Policy 热更、人工 stop、后台作业完成、双实例 Lease 争用、数据库迁移。

恢复验证使用真实子进程终止与重新启动，不只调用内存 reset。外部服务用可记录执行次数的客户端。GitHub/飞书并不普遍提供端到端 exactly-once：可用平台幂等键/消息 marker/查询回查；仍无法确定时保留 unknown，人工或受控对账后再恢复。

### 真实模型和产品验收

先通过脚本化模型，再锁定相同端点、模型版本、推理参数、工具和输入窗口，进行重复真实模型试验。测试任务、重复次数、token/费用上限和放行阈值在 M0 固化；未锁预算不得自动扩展调用。先用只读或专用测试仓，真实 PR 只投递到允许的验证仓。

每次比较记录接单决策、实际动作、交付物、人工干预、消息数、时间和费用；报告样本量、差异及其置信区间。文字不要求逐字相同，但越权、误接单、丢任务、未知副作用重放和伪造成功均为零容忍；不能用少量成功演示证明全面一致。

完整产品需要浏览器操作真实后端，验证全部控制按钮、版本冲突、服务重启、会话跳转和搜索。UI 使用假 API 的组件测试不能替代这层验证。

### 证据结构

每条 rubric 的 pass 必须链接到具体 test ID、双方 SHA、scenario SHA、环境、完整命令、退出码、原始日志、轨迹和差异结果。失败归因不能删除原始失败。skip、环境缺失、网络中断、参考本身异常单独登记，不能涂成绿色；修复后保存复跑证据。

日志不包含 API Key、认证 Cookie、真实私密消息；真实模型请求轨迹只在授权测试环境保留合成内容。模型可见来源与 Host 内部授权事实分开。

## 实施阶段和任务依赖

每个任务提交应同时包含实现、对应测试、规格更新、Rubric 映射和证据入口。可合并小任务，但不能越过阶段退出门槛；从 M0 开始按顺序推进，不先重写 UI 或全部 Agent Loop。

### M0 锁定基线与差异

依赖：无。退出门槛：基线、契约、冲突和现有回归清单全部登记；不允许用未核实的接口开始改循环。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M0-01 | 冻结三个 SHA 和完整源码清单 | 生成 baseline.lock、默认值/Prompt/工具/状态/API/时限清单及引用许可证。 |
| M0-02 | 建立 XHarness 原行为基线 | 按远程规则跑受影响既有测试和 CI 门禁；保存失败、跳过、环境条件，不把原有红灯藏掉。 |
| M0-03 | 逐项登记参考测试 | 将参考全部测试文件和核心断言映射到 rubric；文档与源码差异由实际可观察行为裁决。 |
| M0-04 | 关闭兼容决策 | 处理停止/队列、嵌套子 Agent、角色模型选择、沙箱权限、JSONL 与业务库边界，不默认放宽旧权限。 |
| M0-05 | 建立同步与所有权规则 | 新增自有 crate 加入 OWNED_CRATES；固定同步来源 SHA 的受控方式，禁止本机执行含 cargo check 的同步脚本。 |

### M1 可执行行为参考与比较器

依赖：M0。退出门槛：同一夹具可驱动参考实现并产出标准轨迹；比较器能发现故意注入的错接单、重复执行和越权。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M1-01 | 搭建 TypeScript 参考适配器 | 保留官方核心与校验器；通过测试端口运行命令，不以手写期望替代上游实际输出。 |
| M1-02 | 定义标准事件和快照 | 统一 Trigger、Decision、Ticket、Assignment、输入收据、模型请求、工具调用和 Outbox 事实。 |
| M1-03 | 加入脚本化模型与工具 | 确定性 Provider、时钟、ID、GitHub/飞书响应和进程任务；默认不消耗真实 API。 |
| M1-04 | 实现差分与变异测试 | 仅规范化无业务意义的 ID/绝对时间；保留身份关系、因果、消息次数、错误和授权，错误注入必须红灯。 |
| M1-05 | 固化证据格式 | 每次记录代码 SHA、夹具 SHA、环境、结果、日志与 diff；not_run/skip 不计 pass。 |

### M2 Rust 工单核心与持久存储

依赖：M1。退出门槛：AUTH、DOM 及持久 Effects 的确定性轨迹逐项一致，事务/幂等/恢复测试通过。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M2-01 | 实现纯领域模型 | 新增产品核心 crate，移植 Thread/Ticket/Mandate/Assignment/Subscription，业务核心不依赖 Host、网络或 UI。 |
| M2-02 | 实现准入和策略版本 | 移植确定性 gate、admission table、observe/auto、规则结果验证及 Mandate revalidation。 |
| M2-03 | 实现跟进和结束状态机 | 所有权、handback、上下文订阅、PR 结束、人类恢复、stop 分支全部覆盖。 |
| M2-04 | 实现 SQLite 和 Effects | 领域事务、持久副作用队列、外发意图与收据、版本化 schema；会话内容仍只存 JSONL。 |
| M2-05 | 跑领域差分矩阵 | 将已映射上游断言和参数化组合送入双引擎；任何差异先登记，不修改 goldens 迁就实现。 |

### M3 接入 XHarness 运行时

依赖：M2。退出门槛：RUN、TOOL 的消息/生命周期/作用域契约及第一条脚本化完整任务通过。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M3-01 | 实现产品运行时端口 | ensure/restore、followup/steer/inject、observe/cancel、jobs/subagents、角色工具和 Prompt 接口。 |
| M3-02 | 绑定持久输入与事件 | 先订阅后唤醒，输入稳定 ID 和 receipt；事件绑定到 assignment/turn，正确处理消费者重挂接。 |
| M3-03 | 实现二十个应用工具 | 主会话 14 个与 Worker 6 个；复用同名 read 按角色分权；兼容基础工具 Schema，约束 Host 注入工具。 |
| M3-04 | 实现工单执行资源 | 每工单工作树、prepare、并发槽、5 秒前台转后台、后台结果唤醒、30 分钟与交接回收规则。 |
| M3-05 | 补齐必要底层能力 | 若需修改同步过来的核心，先在 x-harness-rs 单独实现并验证，再受控同步；不直接修改副本漂移。 |

### M4 完整来源接入与交付

依赖：M3。退出门槛：GitHub 和飞书完整来源语义、持久 Outbox、PR 及跟进闭环通过；旧入口回归通过。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M4-01 | 实现 GitHub Source | 通知与 observe 事件、线程订阅、PR/CI/关闭、游标和授权；旧 /xh review 入口继续可用。 |
| M4-02 | 实现飞书 Source | 机器人事件、历史补读、群/私信准入、消息/图片/回复上下文和身份同步。 |
| M4-03 | 实现来源专用账号适配 | 以专用 gh/lark-cli 身份提供参考兼容路径；不假设现有原生 client 与官方账号语义天然一致。 |
| M4-04 | 实现可靠回写 | 每 OutboundMessage 一个幂等身份、持久重试、顺序与 dropped；未知结果先对账，不覆盖同会话的其他消息。 |
| M4-05 | 验证真实交付生命周期 | 明确请求、修复、测试证据、PR、review follow-up、CI、合并/关闭与最终通知，默认不自动合并。 |

### M5 Prompt 配置控制面与界面

依赖：M4。退出门槛：PRM、CFG、UI 全部有真实链路，非截图或空壳；配置和 Prompt 冲突/升级可恢复。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M5-01 | 实现管理接口兼容层 | 保留稳定 /orchestrator/api 契约、revision、错误码、Origin 和权限；UI 私有接口按版本管理。 |
| M5-02 | 实现 Prompt 与技能版本 | 角色装配、原文默认值、每请求生效、占位符、覆盖、base 差异和历史恢复。 |
| M5-03 | 实现完整管理面板 | 六主标签、总览、搜索、工单详情、会话跳转、所有控制操作、中文英文与可访问性。 |
| M5-04 | 实现设置向导和服务运维 | 账号探测、Setup readiness、状态码、独立生命周期、人工停止和健康检查。 |
| M5-05 | 实现升级回滚流程 | 数据库版本、会话兼容性、默认更新、备份恢复及显式导入；不自动迁移用户旧会话。 |

### M6 全量一致性与非回归验收

依赖：M5。退出门槛：所有必需 rubric 和 XHarness 既有门禁通过；故障矩阵、真实环境、跨平台证据无未解释缺口。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M6-01 | 执行完整差分套件 | 参考测试全映射、参数化边界、工具/Prompt wire、领域状态和界面行为报告。 |
| M6-02 | 执行故障并发矩阵 | 真实杀进程、双实例、收据丢失、策略热更、任务停止、背景结束等切点组合。 |
| M6-03 | 执行 XHarness 原有门禁 | 远程 fmt/check/tests/Clippy、架构变异和实际编译器变异；声明平台需原生远程 CI。 |
| M6-04 | 执行受预算约束的真实模型回归 | 相同 Provider/模型/参数与任务集；记录采样差异和置信区间，决策/副作用违约均调查。 |
| M6-05 | 完成全部阻塞差异清单 | 记录每项 pass/fail/not_run/approved_deviation；偏差不能作为严格全面对齐通过。 |

### M7 灰度发布和可回退交接

依赖：M6。退出门槛：影子验证、受控真实试点、回滚演练和最终 evidence bundle 完整；仅此时宣称全面行为对齐。

| 任务 | 内容 | 交付物和验收范围 |
| --- | --- | --- |
| M7-01 | 影子回放 | 两边读同样历史事件，影子输出只进假外部客户端；只有一个主实例有写权限。 |
| M7-02 | GitHub 明确派活灰度 | 一个测试仓库、短允许名单、人工合并；确认不是 mock provider，原会话不受影响。 |
| M7-03 | 飞书和主动模式分级开放 | 先飞书明确派活，再 observe 复核；auto 和自动合并由操作者分别显式开启。 |
| M7-04 | 生成发布证据和运维交接 | 锁定最终 SHA、构建哈希、rubric/test 结果、部署配置、已批准差异和下一步操作。 |
| M7-05 | 演练停服回滚 | 尊重手动停止、等待结算、备份、恢复兼容快照；未知副作用必须对账后再恢复。 |

## Rust 验证和远程执行纪律

任何会编译 Rust 的命令均禁止在本机执行。默认使用 WZU_Server；无 GPU 工作不切换 WZU_4080；不把 RWKV_Ubuntu 当作自动替代服务器。所有检查都先同步当前源码，包括未提交修改，排除 Git、target、node_modules、环境文件、密钥、认证 Cookie 和其他敏感文件。

以下为执行阶段模板，不表示已经执行。LOCAL_REPO 替换为实际源码绝对路径；OUT 替换为本次证据绝对路径。同步不盲目删除远程个人文件；源码中已删除文件须通过受控源码 manifest 清理，避免旧文件污染验证。

```bash
set -euo pipefail
LOCAL_REPO="/absolute/path/to/xharness-runtime"
OUT="/absolute/path/to/evidence"
mkdir -p "$OUT/logs"
ssh WZU_Server 'mkdir -p "$HOME/codex-build/xharness-runtime"'
rsync -az \
  --exclude='.git/' --exclude='target/' --exclude='node_modules/' \
  --exclude='.env' --exclude='.env.*' --exclude='*.pem' \
  --exclude='*.key' --exclude='*.p12' --exclude='*.pfx' \
  --exclude='.ssh/' --exclude='.gnupg/' --exclude='.aws/' \
  --exclude='.azure/' --exclude='.netrc' --exclude='.envrc' \
  --exclude='id_rsa*' --exclude='id_ed25519*' --exclude='id_ecdsa*' \
  --exclude='credentials*' --exclude='.credentials*' --exclude='cookies*' \
  "$LOCAL_REPO/" WZU_Server:codex-build/xharness-runtime/

ssh WZU_Server 'bash -s' 2>&1 <<'REMOTE' | tee "$OUT/logs/rust-runtime.log"
set -euo pipefail
cd "$HOME/codex-build/xharness-runtime"
cargo fmt --check --all
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo test --locked -p xharness-process --test process dropping_ -- --test-threads=1
REMOTE
```

修改 x-harness-rs 核心时，先同步到远程 codex-build/x-harness-rs，再按该固定基线 CI 执行架构检查、架构变异、实际业务错误编译器变异、受影响测试和严格 Clippy。含编译器调用的 Python 检查同样只能远程运行。[现有质量门禁](https://github.com/123123213weqw/x-harness-rs/blob/4ab403e0bb671b69fe56c5355bcf10dad7010c0b/.github/workflows/ci.yml)，[项目指令](https://github.com/123123213weqw/x-harness-rs/blob/4ab403e0bb671b69fe56c5355bcf10dad7010c0b/AGENTS.md)

Linux 在 WZU_Server 验证；声明 macOS/Windows 支持时还必须通过对应原生远程 CI。交叉编译成功不等于原生行为通过，没有对应环境时保留 not_run，不能用本机 Rust 测试补洞。

SSH/网络暂态失败先定位解析、连接、认证和远程工具层，使用有上限的退避重试及健康授权连接；保持主机密钥/TLS 校验，不切换未授权主机。确需外部条件时报告未完成及下一步，不回退本机编译。

业务代码保持现有 unsafe 与 recoverable error guards；新增受管模块同样纳入。禁止以静默 .ok()/默认值掩盖存储、任务或外发失败。

## 发布门槛和回滚

### 放行条件

- 所有必需 rubric 有结果和证据；P0、P1 全通过，未执行/跳过/未决偏差为零；现有 XHarness 回归无新增失败。
- 参考的全部测试文件与重要断言完成映射，新增差分和变异用例能真实发现破坏行为。
- 明确区分产品已经支持、仅 Runtime 有底层接口、仍未装配的能力；不存在因编译通过而宣称功能已完成。
- GitHub、飞书、主动三模式、工单多对多、版本配置、Prompt 覆盖、面板操作、重启/停止/恢复均走真实链路。
- 缺模型配置、缺依赖或无平台能力时明确拒绝；生产不会静默变 mock。
- 业务库和会话日志升级/回滚已演练；只有一个实例拥有真实外发能力。
- 发布构建、最终 SHA、依赖锁、场景清单、费用记录和操作手册齐全；没有自动扩大名单、启用 auto 或自动合并。

### 灰度顺序

历史事件影子回放 → 测试 GitHub 的明确派活 → PR 与后续 review/CI → 飞书明确派活 → observe 决策复核 → 操作者显式开启 auto。每步失败退回上一稳定层；影子实例不得给真实 GitHub/飞书发消息。

### 回滚步骤

1. 暂停新接单和外发调度，记录进行中的 Assignment、输入收据与未知副作用。
2. 等待或显式停止运行工作，保存业务库、会话日志、工作树、Outbox 和配置快照。
3. 关闭新产品入口，恢复原 gh-host/feishu-host；不把新工单自动灌进旧会话。
4. 若数据库已迁移，恢复与旧二进制匹配的快照，不能让旧版本强读新 schema。
5. 对未知 PR/评论/工具副作用先核实；没有证据时不盲目重跑。
6. 记录回滚结果，人工解除暂停后才恢复明确授权工作。

## 下一次执行的起点

从 M0-01 开始，而不是先写新的聊天界面。第一批产物应为 baseline.lock、capability matrix、现有回归证据、默认 Prompt/工具快照、差异决议和最小双引擎夹具。M1 通过后才移植 Rust 领域核心。

最终交付不仅是可运行程序，还包括可重放的行为证据：任何后续版本都能运行同一 rubric，证明产品继续对齐且旧 XHarness 没有退化。
