# B2：工单与线程控制接线

本批在正式产品的同一 Native 工具工厂中新增7个处理器，不是只注册名称。
当前应用工具是**主角色6个、Worker5个，共11个子集**，仍未达到主14/Worker6。

| 角色 | 新工具 | 实际动作 |
| --- | --- | --- |
| 主角色 | `orchestrator_ticket_retry` | 核心重试命令结束旧 assignment、建立新 assignment，持久化停止/启动意图 |
| 主角色 | `orchestrator_ticket_request_status` | 只给正在等待状态报告的 Worker 加状态请求；其他状态不发送 |
| 主角色 | `orchestrator_trigger_requeue` | 重新排入研判；仍由 Worker 决定的 follow-up 必须先由它 handback |
| 主角色 | `orchestrator_thread_unsubscribe` | 去除线程上的全部工单订阅；重复调用返回未移除 |
| Worker | `orchestrator_thread_follow` | 仅为当前工单订阅上下文，不授予新工作权限；重复调用不新增通知 |
| Worker | `orchestrator_thread_unrelated` | 只去除当前工单的无关线程；来源线程和绑定 PR 线程受保护 |
| Worker | `orchestrator_trigger_handback` | 当前工单将已到达的消息交回重新研判；保留归属及一次性规则 |

Worker 的工单 ID 来自可信 Native 绑定，不接受模型传入另一个工单 ID。角色工具集
仍隔离，主角色命令不会因为 Worker 猜到名字就执行。Schema 来自固定原版的已有
运行时契约；原始元数据和参考源码未被改写。

## 状态、队列和通知

所有命令仍进入同一个 leased SQL owner，复用现有 Engine/Driver/Passes，不增加
会话数据库。重试不会把旧 assignment 改个状态就当成新执行。

重新研判前的所有权检查先完成可失败的点读，再调用原有纯判定函数；不能把一次
数据库失败当成“没有 Worker 决定它”。线程 follow/unrelated 的状态与通知意图在
同一事务里提交，失败/重复调用不会增加通知。通知采用持久化发生序号派生身份，
通过 Native 的被动 inject 写入原会话，不伪造模型回合或另起聊天历史。

本批还修正了一个实际状态接线缺口：Core 的 `turnEnded` 只记录 idleAt，Executor
还应在普通安静回合结束后记录 `waiting`。现在未交给人工、无活动 process job、
且没有跨读取边界的新输入/报告时，会保留最后文本或原摘要，记录 waiting，并按
原版规则通知主角色。主角色随后能真正请求状态，消息到达**同一 assignment**。

**后台任务完整结算尚未完成。** 检测到活动 job 时不会伪造 waiting，但原版30分钟
后台/子 Agent 等待、超时停止、slot 与任务完成通知还须继续接入和验收。这不算
WORK-04 通过，也不把“无后台任务”的成功外推为后台生命周期已完成。

## 关闭竞争修复

前一提交的 Linux CI 实际复现了 Native prompt 关闭竞争：门禁已经关闭，而 Core
的取消命令尚未处理，导致原本应取消的回合被记成 Failed。产品自有 resolver 现在
只对明确的关闭门禁拒绝、或停止期间已准备成功的请求等待其**原有取消令牌**，
让 Core 负责结算；不改写内核状态，也不会在停止期间继续调用模型。

真实 SQL/Host/base-prompt 错误不按此处理，仍保持原类型返回。原有 Failed/Cancelled
断言没有放宽，冻结的 Host 导入文件和参考断言也没有修改。定点重复验证不替代
新提交的完整跨平台 CI；旧 Host 测试还曾出现独立的 flush 观察竞争，需按具体结果
处理，不能通过修改固定参考或删除断言来凑绿。

## 工作区回归边界

本批 workspace fmt/check/严格 Clippy 通过。完整 workspace 测试实际执行但未通过：
旧 `xharness-feishu --test sandbox` 的专用 UID、出站过滤两项部署断言失败，
实际 UID/GID 为1000/1000而非1007/995，负向探测观察到200。保留完整日志；没有
改变服务器账号、权限、防火墙、代理或服务，也未删改断言。不将其记成全工作区绿，
也未做基线重放来宣称已证明与本次改动无关。

## 已执行的产品路径与边界

正式入口的外部模型 HTTP/SSE 和 gh 仍是明确离线协议夹具，其他环节为实际实现。
本批新增的路径包括：

1. 显式请求允许失败后重试：第一次真实 Worker 报告 failed，主工具建立第二个
   assignment，之后才调用实际 write，最终 ready/回复；同一工单保留两次执行事实。
2. Worker 上下文 follow、重复 follow、无关线程移除、来源线程保护、非法 handback、
   额外工单参数/跨角色调用拒绝，之后实际写文件并报告。通知不是额外模型任务。
3. 无报告的安静 Worker → waiting → 主角色状态查询 → 原 assignment 收到消息 →
   实际写文件并完成；不是只检查工具返回“发送成功”。

另有3个明确的组件测试覆盖多工单线程隔离、合法 handback/重新研判、状态/重试，
以及数据库失败不变成无所有者；这些不单独计作整产品验收。

仍缺：两角色共用的完整 bounded `orchestrator_read`、主角色 note/3个 eval 工具、
settings/policy-sync/prompts 管理工具，以及完整 Source 上下文、飞书、API/UI、恢复
和部署。没有给它们注册假成功处理器。完整 Goal 保持活动，不把这11个工具当成
全部20个工具或全部业务完成。源码、命令、日志和当前验收范围见
`docs/business-controls-verification.json`。
