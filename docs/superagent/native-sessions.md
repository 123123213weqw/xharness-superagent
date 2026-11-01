# 原生 Sessions 接入边界

这是实际 XHarness Kernel 的 **队列/收据/观察者与被动notice组件**，不是完整 SuperAgent Runtime。最终 RUN/TOOL/REC Rubric 不因组件通过而计通过。

## 已接入的真实能力

- `DurableLoopAgentRuntime`、`JsonlSessionStore.for_runtime`、`FileLeaseManager`，以及 JSONL control log 和持久 question authority。会话与 Inbox 只有 JSONL 一个真源；SQLite 只拥有领域状态和待发意图。
- 私有 BasicHost 仅承担可信 Workspace/ScopedSessionCreate 和原始创建收据验证。`CreationAuthority` 不接收输入、不恢复执行、不绑定 live Runtime，不作为 HTTP/UI 的会话投影。否则 generic Host 会在每次准备请求时覆盖产品的角色模型和 Prompt。
- `ManagedSession` 是不可反序列化的可信能力；逻辑角色和实际 native Session ID 分开。主/Worker 的路由和工具 Factory 由部署端提供，不由模型参数决定。物理身份只取决于逻辑角色；原始创建收据验证 workspace，私有 owner 能力防止跨实例复用。仅移动 state 存储、保持 workspace 路径不变时，不新建会话或丢失收据；这不是整机/工作区迁移验收。
- 角色默认模型仅在没有已选模型/历史请求时播种；已有显式/持久选择保持。缺少已配置角色模型时直接报不可用，没有静默 ScriptedProvider 或其它模型回退。
- `prompted_id` 使用参考 `dsh-superagent:key(requestId)`。queue 在持久 Inbox admission/flush 后返回；重复 ID 保留原输入、不替换内容，也不单独创建 SQL 消息账本。
- 唤醒是显式操作。冷恢复通过 `resume_session` 重新订阅原有持久输入，再取 input-scoped observer；不能调用冷 `start_turn` 重新准入同一输入。其后由原有内核 claim/turn-start/user-message/settle 驱动，不新增聊天循环。
- 输入来源明确标为 `dsh-superagent`，不是 user。failed/stopped 会话的自动派发门禁保持；产品关闭、Assignment cancel 和人工暂停的完整不同语义尚未实现，不能直接套用宿主 Cancel。
- Role Factory 和 Host 二次注册 seam 都受约束；在实际产品工具接入前禁止泄漏 generic Goal/schedule/agent/user-question 工具。未知子会话不被自动收编。当前不能安全表达的自定义 sandbox/approval override 失败关闭，不扩大权限或禁用审批。
- 整个产品层关闭共享同一结果。内核 worker 的 graceful 报告本身不代表 OS lease 已释放：关闭后释放 owned Runtime 的强句柄缓存，再在同一 grace 内验证实际租约。外部仍持有 RunningTurn 时诚实报告未释放，不清锁文件、不碰持久 Inbox。

- 新增 `inject_notice`：通过现有 `prepare_automation_worker` input-free准备actor，再调用 `AgentHandle.inject`；NativeSessions没有Schedule服务，不创建自动化/Goal/假输入，idle不wake、active不interrupt。raw notice键与prompted_id的hash层分开；已取消notice的live seen只存内存，重启依据JSONL pending/consumed事实。native消息来源从Inbox provenance按消费ID关联，不用ID前缀猜来源。5项新增真实Kernel测试使总计22项；完整范围见delivery.md。

## 尚不能据此宣称的行为

1. 真实followup/非中断steer/passive inject原语已接入，但完整AgentDelivery回调/持久化/授权Facade、Sessions notice/relay、MasterAssessor、DshExecutor 和模型预算重试健康仍待接入。测试中为验证 SQL→JSONL 边界而安装的 queue-only relay handler 不是生产 steer relay。
2. 注入到测试的 provider/Factory 是明确的离线探针。没有真实 provider wire、GitHub/飞书 Source 写入，也没有安装 14+6 个应用工具或基础工具/skills。
3. Prompt 测试仅证明传入的角色 assembly 未被 generic Host 替换；版本权限、Mandate、AGENTS、完整 provider envelope 仍待验证。
4. jobs/children 的 working/idle、并发槽、人类优先、交接与回收，以及完整 Setup/Source/root-lock/stop 生命周期仍未提供。
5. 原有 DSH 的输入合并/step 调度、移除和旧消息源兼容，live 模型/策略切换，以及大历史/冷存储性能和故障切点仍需逐项差分；不能从 native kernel 自身的正确性推导参考行为一致。

## 验证

`bash scripts/superagent/remote-validate.sh native-session-contracts` 会先按受控规则同步源码，再仅在 WZU_Server 执行 format/check、Clippy 和原生测试；本机不编译 Rust。组件的最终结果与范围见 `conformance/reference/native-session-target-results.json`。所有未决差异（包括 64 条未来时区和 55 处通用迁移编码）保留，96 条最终 Rubric 仍未验收。

新增8项真实测试后Native共30项：非中断step idle/live、双input observer、ordinary分turn、owner移除重投、冷恢复订阅、live notice去重及旧RPC内部重试不重开门禁。上游能力与11路径受控backport见kernel-follow-step.md；没有改变旧全局steer/cancel默认。
