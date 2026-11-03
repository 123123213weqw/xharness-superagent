# 飞书文本事件：正式产品闭环

飞书文本事件已进入同一个正式 `xharness-superagent` 产品入口：只读验证的 bot CLI
→ 实际受管事件进程 → Source → 策略/研判/SQL → Ticket → Native Worker
→ CodingToolBundle 实际写文件 → report → Main reply → 飞书 Outbox 回执。
这不是旧 `feishu-host`，也没有新增 SDK、Node 服务或聊天历史镜像。

**范围：文本及所列离线协议场景，不是完整飞书、更不是全部 SuperAgent 业务对齐。**
模型 HTTP/SSE、GitHub 和飞书服务端点使用明确的离线协议夹具；正式程序本身不含
固定模型答案、假写文件或假回复成功实现，未向外部服务发送消息或调用付费模型。

## 本批实现

- 持续模式由产品 owner 同时拥有 GitHub 与飞书；飞书事件、历史/订阅轮询、群名
  刷新均在该生命周期内。停用来源五秒内取消并等待实际事件进程及其子孙；入口
  中断后才释放状态租约。`--check` 不启动循环；`--once` 执行一次历史/名称读取。
- bot/app 自身事件直接丢弃；有效的范围外消息仍进入领域层并保留 filtered 事实。
  来源停用后已读的事件丢弃；未入名单的机器人是策略 ignore 决策，不伪装成范围过滤。
- 无换行/分段正常 Unicode、坏 JSON、超长行、ready/退出、退避和取消使用实际进程
  协议。超长行只诊断并丢到下一换行，同一消费者继续接后续消息；输出观察器真实
  丢数据仍报错重启，不把不完整上下文当成功。重启退避为1秒到60秒。
- 群/已知私聊历史按时间补拉，初次只种游标；话题和订阅线程使用既有 SQL/KV
  游标/seen/labels，不另存消息正文。页面不得越群/越话题，游标必须前进；非法条目
  跳过并计数，失败或取消不推进轮次游标，成功留五秒重叠供幂等去重。
- 真实资源读取支持 message/root/parent/thread/around、原版分页游标、rich 文本
  投影和链接。Main/Worker 仍通过既有读取规划器检查 Trigger 可达范围；上下文
  读取不变成新的主动消息。删除、跨群、错误页和不完整响应均不假造空白成功。
- Prompt 与准备/Worker 编码环境使用当前设置及实际 bot 非秘密信息。飞书 CHAT
  只来自工单起源 Trigger，不被后续跨来源输入改写；GitHub 全局只读准备仍必需。
  Channel guard 每次取当前设置，飞书回复100词业务限制在提交前校验并复用原 bot。
- 不放宽 WorkerNetworkDeny、TicketAuthority、Host 默认工具/权限或用户身份。

## 实际验收而非组件代替

新增6条正式二进制场景；完整正式入口测试为43条（不累加重复执行）：

1. 事件重复投递 → 1个 Trigger/工单/assignment → 真实 message/thread 读取 →
   中文/emoji 文件和准备环境 → ack/result 回执 → 中断重开不重复执行/回复。
2. 坏 JSON、自身 bot/app、范围内/外人类和外部机器人：保留不同的丢弃/filtered/
   ignore 结果，不启动未获授权 Worker。
3. 私聊关闭时保留过滤事实、开启时实际运行并使用正确私聊回复地址。
4. Main 保存来源关闭后，实际消费者及子孙停止；新读事件不接单，不后台重启。
5. 实际消费者首次退出7 → 等待至少1秒 → 重启 → 同一请求只完成一次。
6. 历史越群/卡页 → 错误可见且原游标保留 → 修复下一轮 → 已完成请求不再唤醒，
   新游标按轮次开始时间减五秒保存。

另外54个直接调用未修改原版 reader/Commands 的协议样本验证正文、上下文、链接、
资源/游标边界和投影，并严格核对 Rust 的实际 CLI 调用参数。这些是组件差分，
**不算54条产品验收**；没有把未调用的图片函数算作图片通过。
超长行的实际进程继续/回收与两个窄适配单测同样只是补充组件证据。

本批还记录先前646b6df的 Mac CI Worker 重试失败：已终结回合的 Closed 被 durable
Host 包成 Rejected。产品观察者只认这个精确封装，仍排空并 join 真实 result；其他
拒绝仍返回错误，没有改 Core/Host 默认语义。Linux 同一正式重试路径额外重复20次，
不冒充 Mac 已验证；新精确 head 的 CI 另看 GitHub。

完整成功/失败日志、原/公开 SHA 和源指纹见
[本批验证](../business-feishu-live-verification.json)。原版216文件、受控11步/86路径
和10条审计 guard 保留；Cargo.lock 与 NativeSessions 未变，没有新增依赖。

## 如何使用这个开发路径

在 `settings.sources` 与 `settings.policy.sources` **合并**以下片段，保留完整模型、
状态和全局 GitHub 账号/仓库配置。所有 ID/路径必须替换；授权名单是用户选择，
程序不会自行加人。新配置文件的 settings 仅首次初始化，不能覆盖已有数据库配置。

```json
{
  "sources": {
    "feishu": {
      "enabled": true,
      "config": {
        "cli": "/absolute/path/to/bot-lark",
        "chats": [{ "chatId": "oc_REPLACE", "listen": "mentions" }],
        "directMessages": false
      }
    }
  },
  "policy": {
    "sources": {
      "feishu": { "allowed": [{ "id": "ou_REPLACE", "name": "成员名" }] }
    }
  }
}
```

对已在远程构建好的二进制运行 `--config /absolute/path/config.json --check`，
确认 bot-only 配置和健康后持续运行（不加 `--once`）才会消费实时事件。示例配置
仍是 observe/来源关闭；研判模式、准备脚本和授权需用户明确配置。这里不是线上
可部署性承诺，真实企业/飞书权限、服务网络和模型能力未做外部联调验收。

## 下一步仍在活动 Goal

- 图片资源、上下文图片/附件与真实 Native attachment 生命周期尚未接通。
- 本批未签收全部订阅/话题/名称变化、来源并发、权限撤销、退出码/SQL 故障切点；
  六条正式场景不能代替完整 Source 错误/恢复矩阵。
- 全部工具细分校验与 PR/review、跨来源/完整研判分支、配置 API/UI、后台任务、
  完整停止/并发/崩溃恢复及部署/备份/回退仍需执行。
- 23组清单仍为5组离线通过、10组部分推进、8组未运行；不以工具名或组件数计算
  功能完成率。Goal 保持活动，已取消的全域 JS/UTF-16 语言语义工作不重启。
