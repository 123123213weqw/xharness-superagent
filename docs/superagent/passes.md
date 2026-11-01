# Passes 协调器

`passes::Passes` 是真实 Rust 产品协调器，不是对 TypeScript Passes 的调用包装。`Host::driver` 接实际 SQL Engine 和原生 Effects：每轮先 kick，再调用保持逐线程提交边界的 `Driver.pass`，不把整个周期包进一个大事务。

## 语义

- 构造不启动工作。`start` 才创建2000ms周期，第一次不立即执行；configured/closing在pump入口检查，kick后再次检查closing。paused策略仍交给Engine处理，不能跳过其暂停记录。
- pump的kick/evaluateDue同步前缀立即执行；完成、finally、Promise式adoption和coalesced followup是本地FIFO完成任务。并发调用合并成一个后续pass，前一轮失败仍执行后续轮。重入时不提前设busy标志、不持有RefCell或SQLite借用。
- 只由request更新runtime health：失败使用`A pass failed: ${String(error)}`；成功只恢复error，且lifetime已abort时不恢复。直接pump不替代request的健康回调。
- receive在Engine写入前检查closing，只有stored才request。duplicate、filtered、存储失败不启动pass。
- idle等待当前pumping/pumpAgain的all-settled，再等待Effects idle，并重查是否又有pass。stop只停止最新timer并等待当时pass的all-settled，不负责Effects drain，不是closing开关，也不改变产品cancel或持久Inbox语义。
- 参考的重复start会遗留旧interval，stop仅clear最新handle；这里明确保留并有差分断言。正常Runtime只start一次。Native owner最终释放Runner时才销毁其全部本地任务/timer。

## 完成通知不是一次全排空

一次性排空整串完成任务会把后续合并request的健康错误提前到前一个pump的await调用者之前，甚至改变idle返回时可见状态。保留的红→绿差分与原生断言覆盖了这一点。

完成通知使用排队的native delivery边界；ready/外部唤醒的idle Future及时被poll，已有调用者和Effects owner有机会在后续reaction前继续。不是用固定sleep拖延健康，也不是在JS façade补写目标状态。Runner在SQLite owner的LocalSet上运行，所有await发生在数据库/队列借用之外。

## 验证范围

`bash scripts/superagent/remote-validate.sh passes-contracts` 同步后仅在WZU_Server构建/Clippy/测试。测试传输需要显式`conformance` feature以暂停Tokio时钟；默认产品feature不包含测试bridge。原版`tests/passes.spec.ts`4条断言保持不改；额外19条参考/目标录制端口差分包括正向计时、setup/closing、错误、合并、重入、停止、held idle和await可见性。JS只供应原始端口与虚拟时间，不计算Rust的pump/health结果。

16项原生测试另覆盖实际2秒时钟推进SQL、周期kick重试到期Effects、SQL后线程失败保留提交前缀、健康恢复、等待/释放和Runner掉线。测试的assessment handler只是队列协议探针，不是已实现的MasterAssessor或真实模型。

这不是完整Runtime验收：三种消息语义、角色工具/Prompt/skills、MasterAssessor/DshExecutor、后台作业/子会话/槽位、Sources、API/UI、root lock和部署退出顺序仍需接入。JS宏/微任务的所有任意交错、全产品多owner顺序也不能从有限Passes差分外推。96条最终Rubric仍不计通过。
