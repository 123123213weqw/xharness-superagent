# SQL 仓库与异步驱动边界

## 仓库

产品 Engine 通过可失败的 `Repository` 访问状态；`MemoryRepositories` 仅保留为默认测试/参考实现。SQL 实现按 ID、线程、open/active 索引读取，直接写入实际变更的记录，不在每个命令前后加载/重写整段历史，也不预载全部外发收据。

- Core 的 savepoint 和端口 checkpoint 同步回滚；读取失败不会被当作“没有记录”或人工权限拒绝。
- 纯 model helper 的回调保留原有短路语义；回调访问失败时返回首个错误，不再查询后续记录。
- 收据 lookup 可失败，并叠加当前命令尚未落盘的 send/withdraw 顺序。
- 未被命令触及的旧记录保留原始 JSON 和额外字段；修改记录时的通用序列化顺序仍属于 D07，未宣布关闭。
- `RepositoryAccess` 仅是诊断，不是产品真源；统计真实 SQLite VM/fullscan/sort、领域读写及收据读。它不包括测试夹具导入/导出，也不涵盖队列内部全部 SQL。
- 全局语义查询（例如不指明工单时搜索 reply 的归属）仍可能读全局工单；不从点查询的规模证据推导所有查询都有常数成本。冷 WAL/大规模活跃集/端到端延迟仍需 M6 验证。

## 驱动

`Driver::new` 返回同线程的 `Driver` 和 `Runner`。构造/注册不会隐式启用执行；调用方在 LocalSet 等单 owner 执行环境运行 Runner，并在配置/权限就绪后显式 start。没有内置真实 Source、模型、自动工作或自动合并。

- 每次 dispatch 分成 plan → 调用动态 limit → reserve；调用 limit、handler、control sink 时不持有 Effects 借用，允许重入读取/命令。
- handler 是真实 Rust Future。先执行其同步前缀，再查询后续 kind 的限额；完成结算在 dispatch 栈之后，保留 generation/seq 栅栏。
- 只有 kick/start/idle/handler completion 或 Runtime 的周期 pass 触发 dispatch；不新增改变参考退避时机的自动重试定时器。
- 外层事务提交后同步释放控制动作并通知队列；回滚不释放事件、取消、缓存或外发。SQLite owner 的提交回调无需 Send，不把本地状态强制跨线程。
- 控制回调和 handler 的错误可观察；handler panic 转为失败，Later 不增加失败次数。健康 watch 不重复发布相同状态，cancel 的健康刷新同样等到外层提交。
- drain 立即停止预留新尝试，异步等待已有 Future。关闭期间失败与等待行保留，成功仍按原 token 结算。Runner 异常退出向等待方报错，保留待恢复行。
- handler 若需引用 Driver，使用 WeakDriver，避免 Rc 环；提交回调也只捕获弱 owner。

## 周期 pass 不是一个大事务

参考 `Engine.evaluateDue` 按线程/工单分别提交。Runtime 应调用 `Driver.pass`：若后一个线程失败，前一个线程已经提交的评估与持久意图保留。`Store.command` 是显式原子组合接口，不能把整个周期 loop 包进去后声称失败边界相同。

## 不计作完整产品验收

真实 queue/JSONL admission/claim/settle 与 input-scoped observer 已通过原生 Sessions 组件接入，见 `native-sessions.md`；完整三种消息语义、角色工具/后台任务、GitHub/飞书、IM 分词、Prompt/API/UI、provider envelope 与部署恢复仍待接入。原生 Runner 的重放不是对任意未知外部副作用“恰好一次”的证明；真实 Sources/Runtime 的收据与幂等边界仍须逐项验证。损坏的非类型化旧记录、UTF-16、通用 JSON 编码及未来时区差异也仍未批准偏差。

完整 Passes 方法与2秒计时协调器现已在owned `passes`模块实现，见 `passes.md`：configured/closing、合并pump、显式kick、健康、idle和stop接实际Driver。完整产品Runtime的安装/Sources/退出顺序仍未完成；`Driver.pass`自身仍只提供领域部分，不自装定时器或自动启动。两 owned crate 的 Cargo targets 均设置 unsafe_code=forbid，真实负向编译验证 local allow 不能覆盖它；该探针恢复原文件后再跑正常回归。
