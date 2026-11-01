# 当前范围说明

[产品范围](product-scope.md) 已取消全域 JS 语言兼容要求。下文旧记录中的 UTF-16、远未来日期等待办仅作历史，不再是当前产品门禁。

# SuperAgent 兼容边界

这些是实现边界，不是“已验收一致”的结论。最终按 rubric 和真实差分证据关闭。

| 决策 | 实现约束 | 当前证明状态 |
| --- | --- | --- |
| D01 停止与队列 | 产品 Ticket stop 撤销该工单后续工作与待处理输入；不改变旧 XHarness cancel 的队列保留默认值。记录产品停止事实后再执行取消副作用。 | Memory/SQL Engine 回放与提交后控制门禁已有组件证据；真实 Kernel cancel/输入撤销/Runtime 组合验收待完成 |
| D02 子 Agent 与后台工作 | 核心具备能力不等于 headless 已装配。产品角色显式装配并限制工具，保持旧嵌套限制；ready 必须经过子 Agent 和作业静默判定。 | Host 与工厂接口已定位；组合与竞态验收未完成 |
| D03 角色模型 | 操作者显式配置的主会话、研判与 Worker 模型是静态配置路径；不得将其当作模型可自行切换或绕过旧人工授权的路径。 | 待角色路由和真实 provider envelope 验证 |
| D04 隔离 | 不放宽现有沙箱或远程编译规则。产品默认隔离与受控出口必须 fail closed；不为测试修改服务器全局权限、网络或生产服务。 | 原有飞书 UID/出口测试在当前环境失败，详见执行状态 |
| D05 双真源边界 | JSONL 仅作为会话和 Inbox 的真源。SQLite 管理业务工单、授权、调度意图和消费/外发收据，只保留会话事件引用，不复制对话另造历史。 | schema 11 的领域/意图原子提交与四个进程崩溃切点已验证；schema 10、直接 SQL 仓库、原生 Future 驱动均有组件验证；实际会话/Inbox/Runtime 双真源组合仍待完成 |
| D06 实际模型表面 | 已捕获 20 个产品角色绑定，但它们不是完整 DSH/provider 工具集。还需捕获基础工具、system、权限、技能、消息来源、分页和错误行为，不能只对工具名称。 | 产品 schemas 已捕获；完整 provider envelope 尚未完成 |
| D07 序列化身份 | 稳定 ID 与策略版本使用原版实际编码，不自行替换为 JSON canonicalization。禁止为新产品全局启用 serde_json preserve_order 导致旧 workspace 行为随 feature 合并变化。 | 字符串数组 key、已覆盖策略对象键序及 binary64 编码差分通过；来源元数据在 Prompt/UI 中的呈现顺序仍待验证 |
| D08 接收过滤与显式手动路由 | 不把接收过滤误写为所有核心命令的统一前置拒绝。原版核心的 receive 可返回 filtered，而 allowed 人类的显式 decide 仍会接单；这不表示来源自动调度会接受该事件，也不证明工具/API 路径开放。 | 已保存核心参考轨迹 `filtered-explicit-direct-route`；还需在 Runtime/工具边界验证产品行为，SRC-03 未判通过 |

原版 Feishu 消费者的 stdout 解析错误与 stderr ready 来自不同流，最后处理的回调可能覆盖健康状态。记录事件实际处理顺序；保留原版失败的测试，不通过改断言、消除差异字段或把已失败测试计通过来“关闭”问题。

当前 JSON 测试边界不能视为完整 ECMAScript 字符串兼容证明：原始解析器暂设 128 层深度，并使用 Rust Unicode 标量字符串；超深输入及转义的非配对 UTF-16 surrogate 仍需在 API/模糊测试阶段裁决和验证，未批准为行为偏差。当前语义模型中的通用 Metadata 仍使用 BTreeMap/serde_json::Value；来源 fact 顺序和 Prompt 内 JSON 文本不得直接假定已与原版一致。

事务观察边界：原版测试的 vi.fn 回调并非数据库记录，prepare 失败时可能保留已尝试的 release/emit 观察。差分记录保留这些尝试；生产 IntentPorts 则必须随失败命令撤销未提交意图和事件，已用独立 Rust 回滚测试验证。不得把观察回调当作已执行的外部动作。

时间实现现已增加 V8 legacy 语法、完整 TimeClip / 扩展年份、固定版本 IANA 时区与 DST/LMT 边界。8 个时区的 110,720 组语法/算术差分通过；419 个 IANA 名称的 241,344 组边界中仍有 64 组 Gaza/Hebron 远未来规则差异，严格测试保留红灯。别名/平台/时区版本、旧 JSON 非字符串时间及非配对 UTF-16 边界仍未批准偏差，详见 `time-compatibility.md`。

Settings 持久化现保持有序编码，保存、读取、历史与重开数据库不得改变 Policy 身份。已有失败复现及原版身份对照；这只关闭 Settings 落盘的键序缺陷，不表示通用 Metadata、来源 facts 或 Prompt JSON 的顺序问题已关闭。

持久队列与异步驱动分层：Rust Effects 选择/预留当前 generation 的尝试，Runtime 必须在事务外执行并按原 token 回写；处理限额、取消后的刷新、提交通知与 drain 均不可被测试夹具的 JS Promise 调度替代为产品验收。嵌套事务的控制胶囊必须在最外层提交后才释放，回滚后永久拒绝。

迁移差分分别断言完整逻辑记录、SQLite 列/索引与身份敏感的 Settings/Prompt/Source KV 编码。通用领域记录的 raw JSON 键序差异另存原始双方编码，属于 D07 未关闭项，不因逻辑对象相等而计通过。

单 owner 边界：SQL 仓库只读写相关记录，提交回调与 Driver 留在同一线程，以弱引用避免生命周期环；不用 unsafe Send/Sync。所有 owned Cargo targets 禁 unsafe，编译器负向变异拒绝 local allow 覆盖。周期领域 pass 分别提交线程/工单单元，后续失败不撤销已提交前缀；完整 Passes 的配置/closing/2秒/coalescing/kick 协调仍待实现。

## D09：撤回错误边界判断与工具顺序实验

2026-11-01：此前 schemas() 辅助接口的错误不证明真实模型工具装配失败。
SDK 的实际 SystemPrompt.assemble() 源码使用 structuredClone 并默认按工具名称
排序；完整装配还未执行验证。撤回“20 个工具无法提供给模型”的结论与兼容修复
问题，恢复原 Registry，不保留据此前提增加的注册顺序模式。原始实验备份保留在
本地，不构成产品验收证据。
