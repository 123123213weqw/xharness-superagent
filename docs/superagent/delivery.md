# AgentDelivery 协议组件与原生接入边界

参考固定为 `6337fc823ce20251c8de8085948165c81aaa701e` 的 `src/app/delivery.ts`。新实现位于 owned `crates/xharness-superagent/src/delivery.rs`，没有改 imported kernel，也没有把测试端口装成生产 fallback。

## 已实现的协议

- `message_id(rawRequest)` 不额外 hash；`Sessions.prompt` 在上层先 `key(requestId)`，notice 则直接使用 `notice:name:key(JSON.stringify(payload))`。不可混用两个键层。
- 以 live Session **对象身份** 而非 durable ID 或 Agent 对象缓存历史；Session key 释放时即时释放 seen 状态，Delivery owner 释放也不反向保留 key。
- 先 flush，再打开只读历史，按全部事件数量推进每页512条的 offset；成功／失败都 await 异步 close。共享首次加载，失败可重试，保留加载期间 live user/message 及先前成功页看到的 IDs。
- 当前 producer、旧 plugin/dsh-superagent、旧 user RPC ID 的去重身份；不改写旧来源，不把内部 user-role 消息当成人类输入。
- send 的前置 abort、历史后的 abort、live Agent 对象校验、authorize、pending 检查、选定 primitive、accepted、最终 flush 顺序。重复输入仍 authorize/accepted/flush；已准入但最后 barrier 失败时不再投递。
- queue/steer 消息标记 relay，inject 标记 notice+summary。inject 调用成功即加入 live seen；普通 pending 输入被取消移除而未消费时可以重投。**不是“任何历史插入一律永久去重”**。
- 传递原失败原因，包括非 Error 抛出值；read+close 双失败保留 SuppressedError 的 error/suppressed 两个原因。
- 同一 LocalSet 显式拥有 Runner；调用的同步前缀立即运行，结果 waiter 被丢弃不撤回已启动工作。Runner 被强制停止则关闭所有 waiter、清共享 in-flight loader，且不再接受新调用。
- HistoryHandle 适配器必须有 RAII Drop：强制终止无法 await disposer，但不允许共享 loader 继续占用句柄。正常路径仍实际 await close；不能把 Drop 当作成功 flush/close 的证据。

## 证据范围

`delivery-contracts` 在 WZU_Server 上同步当前源码后运行 fmt、conformance-feature 全 target Clippy、原生协议测试、Rust bridge 和原版／独立差分。`superagent-delivery-bridge` 仅在显式 `conformance` feature 下构建，不是产品入口。

JS 仅调用原 fixture 的 persistence/Agent 方法、冻结从 Rust 返回的消息、转发事件和外部 Promise 结果；历史扫描、去重、模式选择、授权／确认顺序均由 Rust 决定。原版测试断言没有修改。测试报告中的 callback 计数只覆盖原版测试文件，独立测试数量以 Vitest 两个文件的报告为准。

原生协议测试不是 DurableLoopAgentRuntime 三模式测试。forced-owner reader 泄漏有保留 red→green 日志；不通过删除断言或固定 sleep 隐藏。任意 JS microtask 交错、恶意／畸形 source、UTF16/深度边界以及完整 shutdown 仍未验收。

## SDK、原生被动注入与仍未完成的差异

核查了 reference 安装的 `@deepseek-ai/dsh-agent@0.2.1-alpha.2` 接口及官方 npm 发布的 `@deepseek-ai/dsh-agent-loop@0.2.1-rc.3` 实现（部署文档指定版本）。后者 `lib/index.js` SHA256 为 `cf57e3ecd487ac5c2553455aaa5cef2c6fb41e6bd61be5aa455675d1d7193033`；alpha.2 的这个文件恰好相同，不能由此推断整个包或全部 Harness 行为相同。

| 模式 | 已核实 DSH machine 行为 | 当前 native seam |
|---|---|---|
| queue/followup | next-turn，并 wake；普通输入单独开 turn | NativeSessions.followup 已对接先订阅再ordinary wake；低层queue仍admission-only |
| steer | next-step，并 wake；不取消正在进行的模型请求 | 新admit_follow_step+follow_step已对接非中断step wake；旧admit_steering中断行为保留，不混用 |
| inject | next-step，不 wake；不是立刻 append user/message 历史 | 复用公开 prepare_automation_worker 获取不自动wake的 actor，再 handle.inject；NativeSessions.inject_notice 已接入，未修改imported内核 |

SDK 文档里还规定取消后的 waking 输入改投 next-turn；这项不能用一个 stale active/idle snapshot 处理。现有 native failed/stopped gate 必须保持，不把内部 source 改成 user，不造 Goal，不全局 resume。

进一步核查公开API后发现：prepare_automation_worker 在当前 NativeSessions 未安装 Schedule service 的构造中，只激活actor并绑定路由，不创建Automation、Goal或假输入。不能因名称含automation就忽略这条已有能力，也不能误称“inject一定需要新内核”。

`inject_notice` 使用raw notice键，串行化同Session准入，保留dsh-superagent/notice+summary、producer flush barrier与live seen。Native user/message本身没有source字段，因此按时间顺序将每个消费ID关联到其最近的Inbox admission provenance；不通过ID前缀猜人类／插件来源。去重只取当前Inbox、实际消费以及当前owner的live inject seen，不把已取消的历史插入当永久收据。5项真实Kernel测试验证idle不wake/noGoal、held模型不interrupt且后续step恰好收到一次、paused fence、移除后同owner不重投/新owner可重投/消费后恢复不重投，以及native pending/consumed旧plugin和user RPC来源保留。测试仍是显式离线provider探针，不是DSH完整machine运行或provider wire测试。

原生接入仍有严格边界：setup/已配置role route可用性、未知来源冲突、没有Inbox provenance的外来旧历史、持久化错误与accepted回调的精确顺序、角色Prompt live装配和全生命周期没有因这些测试计通过。整个AgentDelivery三模式端口尚未组合。

Native holds/queue现在按current、实际consumed与live-inject seen去重；owner移除经过Runtime同时清prepared observer，可重投被移除未消费ID。legacy来源和所有任意数据/取消交错仍不是整体完成，实际支持范围见kernel-follow-step.md。

已在固定真实upstream实现并测试非中断step能力，再按canonical patch受控同步11路径，未整体升级基线。下一步是完整native AgentDelivery回调/持久化/授权Facade、Sessions relay/notice、MasterAssessor/DshExecutor、角色工具/Prompt/skills与生命周期；RUN-01等最终Rubric仍not_run。详见kernel-follow-step.md。
