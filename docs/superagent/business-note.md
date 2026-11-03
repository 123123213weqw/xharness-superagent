# 跨工单事实：ticket_note

`orchestrator_ticket_note` 已接入主角色实际工具路径：在同一 Client/SQLite owner
调用现有 `Engine.note`，保存 `TicketInput::Note`，由既有 Native 投递与恢复链路执行。
返回原版 routed 工单卡，不替换 mandate、授权范围、来源消息或 assignment。
Worker 没有这个主角色工具，也没有增加全局权限、运行时依赖或聊天镜像存储。

## 对齐的产品规则

- 确认目标工单存在且未结束；文本先 trim，必须非空，中文/英文上限 100 词。
  空白、超长、未知或已结束目标返回错误，不创建工单、不保存无效 Note。
- 暂停时仍可保存事实，但不会启动 Worker；恢复后按正常输入链路投递。
- 实际 Worker 收到 `Note from the orchestrator, for context only: ...`，可以结合事实
  继续**原范围**的工作；Note 不是新授权。
- `waiting` 与等待人工的 `blocked/failed` 不同。原版 `waitsForPerson` 仅覆盖后两者：
  Note 可以唤醒 waiting Worker 的同一个 assignment；对等待人工的 blocked/failed
  自动上下文只保存，不把它当人工答复、不启动新 assignment。本批正式测试分别
  验证 waiting 唤醒和 blocked 不唤醒；没有宣称 failed 的全部组合也已验收。

## 正式验证

本批新增 3 条正式入口测试，累计 30 条通过。它们经过实际可执行入口、OpenAI
HTTP/SSE 适配器、Engine/SQLite、Native、CodingToolBundle 和 Outbox；仅外部模型
与 gh 是明确的离线协议夹具，没有付费调用或真实外部写入。

1. 暂停接单、有效/无效 Note、恢复、实际投递、读输入/写文件/报告/回复、结束后拒绝。
   从实际数据库只读捕获 Note 前 mandate，完成后严格比较一致；正常重启不重复 Note。
2. blocked 等待人工时，事实保存但不形成待投递输入、不唤醒 Worker、不写文件/伪造结果，
   重启后状态保持。Note 不充当人工回复。
3. waiting Worker 实际被事实唤醒，仍在原 assignment/mandate 下完成原工作，没有新工单。

最初新增测试误把 waiting 当作 blocked，固定模型因重复发 Note 触及有界超时。
核对固定原版后修正测试条件，并增加 waiting 正向唤醒场景；没有修改领域行为或固定断言。
失败日志和最后通过日志均保留在 [验证记录](../business-note-verification.json)。

所有 Rust fmt/check/严格 Clippy 与所列 10 个 crate 回归在 WZU_Server 独立目录完成；
731 个来源文件本地/远端指纹一致。组件、ignored 和能力门控提前返回不是产品验收。
正常进程重启证明也不等于所有事务/派发崩溃切点证明。

## 仍未完成

主角色应用工具目前 13/14、Worker 6/6，框架 skill 另计，名字不代表全部分支完成。
剩余主工具 `policy_sync` 要接真实来源同步：原版 GitHub 没有同步能力，飞书成员同步
涉及来源安装/启用/就绪、实际 CLI、策略版本竞争、健康和退避，不用始终报错的占位注册
冒充完成。完整飞书/多来源、PR/Review、API/UI、后台/停止、恢复与部署回退仍在活动 Goal。

前一精确提交 `8c3d17d` 的 Linux/macOS CI 已通过；它不是当前新代码的 CI 证明。
当前验收与剩余范围以验证 JSON、业务清单和精确提交的 CI 为准。
