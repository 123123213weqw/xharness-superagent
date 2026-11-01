# 模型侧业务读取（部分业务已接通）

本批把 `orchestrator_read` 同时接入主角色和 Worker 的正式 Native 工具注册。
应用工具数量是主7/Worker6，主角色还缺7个注册；Worker的所有名字已注册不代表
全部业务分支已对齐（例如 PR 认领/报告、飞书及后台生命周期仍未完成）。

## 已接入的实际路径

工具调用 → 可信角色绑定 → 同一 Application Client/SQLite owner → 资源权限规划
→ 实际 Native/Source/配置读取。内部 `Plan` 不序列化给模型，不把占位对象当成功。

- SQL 工单、线程、消息、决策沿用已有分页/脱敏与本工单权限。
- `ticket/<id>/transcript` 和 `message/<n>` 被动读取原 Native JSONL，不 ensure、
  prepare 或唤醒 Worker；只投影公开消息，保留工具调用，移除推理、请求头及私有
  provider 状态。没有第二份聊天历史数据库。排队输入仍可通过工单 input 资源读取。
- GitHub description/comments/reviews/review-thread/commit 与 PR checks 使用已有
  受仓库限制的实际 gh Transport，只读，无任意 URL/命令/账号转发。
- GitHub 从 offset 0 取得的快照按可信会话隔离缓存；复制 next_offset 不重复请求。
  未从0读过的会话不能借用另一个会话的快照。缓存为有界易失 LRU，不是历史存储。
- settings 与模型/Prompt 索引是主角色资源；Worker只读当前或曾用的 Prompt 版本。
  使用实际注册模型目录与全局默认/已保存角色选择；不探测或虚构模型网络可用性。
- 交付文本与 input 资源共享 renderer：上下文 Note、原始 Text、恢复和策略变化
  通知含义不同。长 assignment/trigger/input 有有界预览和全量读取引用。
- 关闭取消并等待正在读取的 gh CLI，再释放本产品状态目录锁；不会伪造完成回复。

## 两处真实反例修正

1. 原版 Zod schema.parse 会给缺失 offset 填0；其 toJSONSchema 默认导出的是输出
   Schema，offset 被列为 required。直接交给 XHarness 的输入校验会使最简单 read
   永远到不了 handler。本产品**仅对两个 read 注册**显式投影为可缺省 offset，保留
   default=0、全部数值约束和其它字段；Request 解析实施该默认。不改通用工具校验器。
   这是业务默认值适配，不是声称目标 Schema 与原导出字节完全一致。
2. 相同 name/status 的检查项，原版读取先列字符串 conclusion 再列 null。保留19个
   原 readGithub/Commands 可执行样本中的此反例，修正产品 CI 排序而非删除样本。

会话投影还对照原 `transcriptEntry` 的独立原函数；Node仅擦除其类型，未导入无关
SDK或伪造 SDK 成功。公开工具调用不是推理内容，不能与推理一起被丢弃。

## 验证边界和仍缺内容

正式入口的外部模型 HTTP/SSE 与 gh 可执行文件使用**显式离线协议夹具**；SQL、
Native、原 OpenAI 适配器、实际业务工具/文件写入和 Outbox 均走产品路径。
组件/oracle 不能代替 PR/Review/CI 全部事件整链路或真实服务验收。

飞书完整 Source 未安装，相关资源真实报 unavailable，不返回假空页；完整 reply
引用、附件、PR认领、Prompt保存产生的 preview、多来源配置等仍属后续业务。
主角色 ticket_note、eval/eval_read/eval_decide、settings_save/policy_sync/prompts_save
这7个注册，以及配置API/UI、后台/停止/完整恢复与部署回退均继续在活动 Goal 内。

普通中文/emoji保留，页边界不拆有效字符；不恢复取消的全域 JS/V8/日期/UTF-16
类型迁移。完整命令、源码绑定、历史失败和未验收项见
`docs/business-read-verification.json` 与 `conformance/business/acceptance.json`。
