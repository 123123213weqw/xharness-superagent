# B2：业务读取的权限、分页与只读会话接口（未完成工具接线）

本批不是新的完整产品验收。**`orchestrator_read` 尚未注册到模型工具集**，数量仍为
主角色6个、Worker5个；不能把以下组件检查说成主14/Worker6已经补齐。

## 已落地的资源层

`application::resources` 在现有 SQL owner 上规划读取，沿用固定原版
`src/app/resources.ts` 和 `src/app/cards.ts` 的业务资源分类、卡片和角色隔离：

- 主角色读取工单、触发器、订阅线程、决策；列表最多50项，按相应的新旧/活动顺序分页。
- Worker 只能读取自己的工单；共享线程卡片不展示其他工单或决策。
- 触发器读取检查原始任务、已送达输入、上下文订阅和绑定线程；读上下文不授予新工作权限。
- Worker 的触发器结果移除决策 ID、其他工单 fanout；不可读的相邻触发器和回复父消息不泄漏。
- 工单详情给出有限的执行/输入首页及后续读取地址；Worker 看不到历史 reopen 的决策 ID。
- 分页参数严格校验；旧 event/task/observation 读取引用仍能定位。模型不能通过参数提供
  另一个可信 ticket/session 身份，也不能传入 SQL 或任意 Store 方法名。
- SQL 失败和损坏记录保留为错误，不伪造成“资源不存在”或空页面。

普通中文/emoji 分页可通过 next_offset 还原全文，不把字符截成无效半个 emoji。
这里没有迁移全局字符串类型、日期/时区或恢复已取消的 JS 语言等价范围。

## 明确尚未解决的端口

资源层对完整会话记录、单个输入渲染、Source 链接/上下文、settings/prompts/models、
Source 原文和 preview 返回明确的内部 Plan，而不是假空正文或成功结果。Plan 不提供
序列化实现，不能直接作为工具响应。完成实际 Native/Source/config 端口、会话级快照
缓存、消息过滤及原版业务分支后，才会接入正式工具并新增模型 HTTP/SSE 场景验收。

目前的工单卡片复用既有业务 helper；完整原版 model-retry/PR 等字段仍须随相关端口
补齐，不宣称这批 SQL 投影已经通过完整 readResource 差分。

## Native 只读会话接口

新增 `NativeSessions::inspect_existing(logical, role)`，调用方必须先用产品绑定确认
可信 logical/role。它只返回已经存在的 JSONL 快照，不返回执行能力或 ManagedSession。

接口校验按角色派生的身份、原有 creation receipt 和 Host 的现有证明，不调用 ensure，
不新建工作目录、Session、工具/Prompt/模型注册或执行 lease，不 prepare/wake Worker。
工作目录已不在时仍可读原来的会话事实。未知会话返回不存在；缺失/不合法证明是错误。

实际 Native 检查覆盖：读取排队但未唤醒的输入；关闭后冷启动、不重新绑定 Worker
仍可读历史；无新增模型/工具调用、Session、模型观察条目；缺失证明不能被当作可信
会话。重复 mutation 由原有 canonical Store 在写入时拒绝，未绕开此生命周期约束。
这些是组件边界检查，不是完整 `orchestrator_read` 的产品验收。

## 旧 CI 的测试等待修复

提交35a34a2的 Linux CI通过；macOS失败于旧 gh-host 的 review_pipeline。源码明确
先写模型 TaskOutcome.done，再异步写评论/标签。旧测试只等 done 就断言评论已发布。
本批让假 HTTP endpoint 记录真实写入通知，等待评论及两条标签落地后，再执行原有
所有结果断言；没有修改生产 gh-host 的状态语义，也没有删除/放宽原断言。

这不能代替新提交的 macOS CI，也不能清除之前的冻结 Host flush 观察竞争或 WZU
飞书专用 UID/出站过滤失败。完整业务、必要回归、恢复与部署 Goal 继续活动。

本批源码与命令/原始失败记录见 `docs/business-resources-verification.json`。
