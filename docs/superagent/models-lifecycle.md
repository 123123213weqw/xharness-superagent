# 管理会话的模型健康生命周期

这是 NativeSessions 中实际运行的健康观察器，不是第二个聊天循环或重试策略。构造不启动观察任务；ensure 只登记已授权的逻辑角色会话。只有正常准入/恢复本来需要 actor 时，才用既有 input-free 准备能力安装被动订阅，然后准入/唤醒。查询未知或未加载会话不创建、不激活、不唤醒。

## 内核能力与移植来源

canonical 上游新增 AgentObservation / AgentActivity 和 observe_loaded_session。活动总线只包含 generation、availability 和可选 status，不复制私有 WorkerState receiver、control Sender 或 activation。持有观察器不会续持执行租约；starting/unavailable/closed 不伪装为 idle。Ready/status 交接在同一个观察写锁中读取当前状态，旧 generation 不覆盖后继。只读观察不等于 physical lease 已释放。

受控补丁为第二步 `conformance/upstream/passive-observation.source.patch.gz`，解压 SHA256 `fc98603f9aa4195ead69542529311d2902674cbeb301ed426ba4d5bac8a612c4`，依赖原 noninterrupting 补丁，source before-tree 为 `af6d0b0e5020b136f4c54a21959e0c47ad68a68e`（原 commit + 已审计变更的 Git tree，不是新 commit）。7 个增量路径，唯一三方冲突来自 master 的旧 restart_reply 邻接上下文；只采用新增 Ready 观察块原字节，不导入 Runtime98 不具备的 restart 功能。两个 manifest 串联检查 before/after 与最终 15 个路径，来源证明不等于语义发布批准。没有整体升级 HEAD。

## 产品所有权

- 每个管理会话一项只读观察任务，NativeSessions owns 并在关闭时取消/等待 join；取消查询不 detach 观察器。同步 Drop 只能 abort，不宣称异步清理成功。
- JSONL 是唯一历史。按真实 seq 增量投影，旧事件不重复折叠，不建立 SQL/内存聊天镜像；每 128 个历史事件让出并检查取消。广播 lag 后读真实日志，不编造消息或模型故障。
- 异步读取前借出 activity 并更新 watch 水位；读取/投影后检查 has_changed，不用值相等漏掉 Idle→Running→Idle ABA。最终清理/发布持有短同步读锁，关闭 check→publish 竞态；绝不跨文件 I/O 持有活动借用。
- 活动来源决定是否真有预算等待，未结束日志不等于活跃 worker。缺失 request/header 不伪造恢复；实际同 turn/step/provider assistant 恢复或正常 idle 清除。closing 时不把 idle 当作提前恢复，真实 disposed 最终清除并释放 models 项。
- 代际/对象 token 阻止旧观察回调覆盖替换对象；它仅隔离健康更新，**没有修复旧 RunningTurn 的跨 turn 控制语义**。
- 读取故障以 typed source 留在 owner，公开数据只有失败标记，查询失败而非返回假的健康成功；关闭不会静默吞掉未回收/失败任务。

## 接口与验证范围

`subscribe_models()` 返回被动 watch；`models_health().await` 对已安装观察器执行新鲜读取屏障，不发控制命令。返回实际 native Session ID，以及逻辑名/角色/代际/可用性；models 的状态/文字/at 用原 Sessions.updateModelHealth 直接比较，ready 不保留旧 at。此库接口尚不等于已完成 HTTP/UI/HealthBoard 安装。

验证：原 RetryHealth 29 条 + 独立 48 条（含原 Sessions 私有 formatter 的实际执行）=77；新增内核 5 actor/租约生命周期 +1 loaded-only Host 测试；7 个产品观察/竞态/故障/取消/join 测试；NativeSessions 32 条包含无需调用方轮询结果的预算状态推送、被动读取不唤醒、持有健康订阅时真实租约释放。新内核文件 4 项实际远端编译器 forbid override 反例、34 架构变异及上游受影响 5 包回归另有日志。所有 Rust 仅 WZU_Server。

完整 Sessions 的 Prompt revision/skills、真实人类/审批/Mandate 上下文、原生 AgentDelivery hot 回调与持久化故障次序、MasterAssessor/DshExecutor/20 工具、工作目录/slots/jobs/children、Root/Setup/Source/API/UI/部署仍未完。原中断超时仍未解释；当前组件测试和重试绿色不消除此偏差，96 条最终 Rubric 不计通过。
