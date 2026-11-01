# B5：未完成工单的正式入口恢复（部分）

本批补的是产品恢复接线，不是 JS/V8 或 UTF-16 的语言复刻。

## 实际修复

Native 的 catalogue/journal restore 不会自动重建产品 Executor 的临时 Worker 绑定。
本批先用正式入口重启测试复现：已 waiting 的工单收到新评论后仍有1个 pending effect。
修复后按可信工单 ID、原工作目录和原 Session receipt 重新绑定，派发只投递新的 Input，
不会重放原 assignment、创建另一套聊天历史，或把恢复当成一次人工 retry。

启动时先在关闭的派发门禁下逐个协调 open tickets，再开放 Native/Source/Effects：

- 已 settled 的 waiting/人工交接仍由原有 `assignmentInterrupted` 规则保留；仅重启
  而没有新输入，不会调用模型或建立新 assignment。
- 真正中断的 running assignment 由原有 Engine 结束为 `hostRestarted`，建立新的
  assignment 和恢复提示，提醒检查工作区/历史/外部操作，不能重复已完成操作。
- pending launch 按接受顺序重新排队；已有人工等待/失败交接不靠准备目录存在就
  伪造成新执行成功。pending 兼容重排分支尚无独立产品故障切点验收。

运行中的工单若丢失工作目录，reconnect 不会再执行 prepare，也不偷偷创建空目录。
真实派发错误会记录到工单 `delivery.error`，Input/重试意图保留；关闭、暂停或已有
后继 assignment 时，不把错误错记到它们身上。`--once` 的 pendingEffects 会显示
尚未完成的派发，不把这个负向场景当成工单成功。

## 三条新增正式入口场景

外部 GitHub/模型仍为明确离线协议夹具；入口二进制、gh 子进程、现有 OpenAI HTTP/SSE
适配器、SQL/Outbox、Native JSONL/工具工厂和实际 CodingToolBundle write 都是真实实现。
没有付费模型调用、线上 GitHub 写入或生产部署。

1. 等待工单正常关闭并再次启动，没有新输入时不唤醒；随后同线程新评论进入同一工单、
   同一 assignment，实际写文件/ready/回复。再次重放同评论不增加请求或回复。
2. Worker 模型请求到达后，对仅属于夹具的正式子进程实际 SIGKILL。再次启动结束旧
   assignment、投递恢复提示、保留工作区的已有部分文件，实际完成新工作；原确认和
   最终回复不重复。这只覆盖无活动 shell/子任务的一处真实进程中断切点。
3. 等待工单重启前移走原目录，新输入不丢失、没有 delivered/read 假确认，没有模型
   调用/成功回复，工单和 pending effect 保留可观察错误，原目录内容不变。

恢复测试为第二个阶段显式配置新的 loopback fixture 端口，以结束被杀进程原先挂起的
HTTP 连接；这不是产品实现内置的故障转移或模型 fallback。

## 回归与未完成范围

前一控制提交的 macOS CI 暴露 HTTP 夹具 socket 的 WouldBlock/EAGAIN 错误。为排除平台 socket 模式继承差异，现明确
设置 accepted socket 为阻塞模式，仍保留有限 read/write timeout 和所有错误断言，
没有忽略网络错误或把失败响应当成功。跨平台是否通过仍须看新提交的实际 CI。

同一旧提交的 Linux CI 还再次触发冻结 Host 测试对 flush 计数的观察竞争；没有编辑
其断言、导入链或通过排除测试来凑绿。上一完整远程 workspace 测试中旧飞书的两项
专用 UID/出站过滤部署断言仍未修复，不能用选定 crate 回归替代全工作区绿色声明。

完整后台/子 Agent 的30分钟结算、停止与 slot/controller、事务/外部动作所有崩溃切点、
人工会话中断恢复、其他来源/授权分支、备份/部署/回退仍待完成。当前主6/Worker5工具
数量未变化，bounded read、剩余业务工具、飞书、API/UI 也仍在活动 Goal 内。

本批源码、精确命令、实际结果和原始失败记录见 `docs/business-recovery-verification.json`；
组件或范围外测试不另算完整产品验收。
