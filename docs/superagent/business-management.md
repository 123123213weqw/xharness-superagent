# 文本首发管理入口（部分对齐）

本批新增真实 HTTP API 与最小管理页，不是另一套工单数据库或执行循环。
所有查询/控制进入已有 `Client` 和 leased SQL owner；Native/Driver/Passes/Outbox
仍是同一条产品链路。**这不是完整原版管理 API/UI 签收，也没有进行六实例生产切换。**

## 启用

参照 `config/superagent.management.example.json`，在现有正式配置中增加：

```json
"management": { "port": 13092 }
```

以默认持续模式启动 `xharness-superagent --config /绝对路径/config.json`，访问
`http://127.0.0.1:13092/orchestrator/`。没有 `--watch` 参数；不带模式参数就是持续模式。
管理功能默认关闭；`--check`、`--once` 不监听端口。只允许 IPv4 loopback，不能通过
配置指定 `0.0.0.0`。端口占用在启动接单前失败并清理 owner，不先接收/回复工单。
远程浏览时使用现有 SSH 验证路径和**同端口**本机转发，不公开监听。

API 保留原版同源门禁：Host 必须是所配端口的 `127.0.0.1` 或 `localhost`，
必须带 `x-orchestrator-request: 1`；如有 Origin，必须是同一个 HTTP origin。
没有增加 CORS 放行、账号/权限提升或 Worker 的网络出口。
响应 `no-store` / `nosniff`；页面禁止嵌框，任务文本使用 `textContent`。
普通请求体上限128 KiB，Prompt 请求按已配置 `4 * promptMaxChars + 4096` 限制，
读请求体5秒超时，停机时取消读 body；HTTP 有10秒有界排空，失败不会伪装成功。

## 当前路由与边界

- `GET state`、`overview`、`sources`：真实 setup 账号、运行健康、负载、来源状态、
  SQL 流量统计、人工处理工单及已实现的异常视图。符合现有 fleet panel 读取字段子集。
  `state.capabilities.fullReferenceViews = false` 明确表示视图尚不完整。
- `GET settings`、`models`、`prompts`：版本化配置、模型注册目录、团队 Prompt。
  身份相关设置及团队技能键的已存顺序不在 HTTP 序列化中重排；没有扩张全域 JS 语义。
- `GET tickets` / `tickets/:id`：阶段复用核心判定；历史结束工单在 SQL 中分页，
  活跃工单按核心工作集筛选。详情/列表是当前文本产品视图，不宣称原版完整装饰字段。
- `POST tickets/:id/stop`、`retry`、`note`：实际 Engine 命令/持久化/资源控制，
  不以 HTTP 成功代替 Worker 停止或工单完成。
- `POST executor/pause`、`triggers/:id/requeue`、`threads/:key/unsubscribe`、
  `decisions/:id/dismiss`：已有业务 owner 上的人工操作。
- `PUT settings/:block`：版本 CAS，复用已有配置/模型/授权校验与策略副作用事务。
  Panel 允许修改后续工作的 prepare/directory/blockedCommands；模型工具仍不能这样修改。
  来源开关可保存；账号/仓库/CLI 变化先用现有 CLI 做只读验证；失败不提交设置。成功后在 CAS 保存后停止并回收旧 owner，再从持久化设置受控重启。不是向活动 Worker 热换凭据。
- `POST policy-sync/:id`：复用已有 bot-only 同步；没有改成新账号或更宽权限。
- `PUT prompts`、`POST prompts/preview`：原有语义校验/版本冲突；Panel 保存来源记为
  `panel`，通知与版本在同一事务持久化，返回 pending 不虚报已注入；工具自改仍不自唤醒。
- 其他尚未实现的路由返回404，而不是空成功结果。搜索、线程/时间线、决策列表、labels、Prompt 历史及 setup/check 已有实际处理器；完整原版装饰字段和所有面板交互仍不宣称逐项签收。

新增 Panel 端口只由 HTTP 组合根安装；模型工具参数不能选择 Panel 来源或人工权限。
没有修改主14/Worker6个应用工具、通用 Host 的默认权限或现有 Worker 网络隔离。

## 验收证据

详见 `docs/business-management-verification.json` 和对应完整日志。
新增4条正式入口场景使用真实二进制、HTTP listener、SQL、Native 与受管子进程；
只有模型/gh 外部端点是明确离线夹具。覆盖：

1. 真实状态/来源/配置查询；缺 header、恶意 Origin/Host、坏路径、过大 body 拒绝；
   慢 body 期间 SIGTERM 退出，端口关闭后状态租约可重开。
2. 人工修改 executor，旧 revision409且不覆盖；伪造 origin 拒绝，账号变化改为只读预验证后受控重启（不是活动 Worker 热切）；
   pause/Prompt/技能顺序持久化并重启生效。
3. 已有 waiting Worker 的实际停止：原 assignment ID 保留且 ended，不另建工单；
   结束工单不能重试，历史分页能读到相同工单。
4. 端口被占用时，即使配置了启用的 GitHub 来源，也不能先 poll/开单/回复；
   `--check` 不抢端口，外网监听字段和0端口配置被拒绝。

另在 V100 新建独立空状态，以现有授权 GitHub bot 做只读 setup，实际通过 HTTP
读取7个端点并用浏览器操作暂停/恢复及配置读取。该探测特意使用无效模型凭据/端点，
两个来源均关闭，不调用真实模型、不消费飞书消息、不执行 Worker、不写 GitHub。
这是账号/API/UI 冒烟，不是代码交付或飞书任务闭环。

首轮实际浏览发现：Passes 只记录失败/恢复，不能据其空 health 说“仍未完成首个 pass”。
现在正式入口仅在实际账号设置、Native restore 和启动成功后初始化 ready；若已有
错误则保留，不覆盖错误，不改 Passes 的参考行为。

## 仍需完成

- Worker clone/build/push/草稿 PR/review/受控远程执行的真实交付通道。
- 管理 API/视图剩余能力、完整 UI 操作验收及账号更换的受控重启。
- 六实例配置/部署覆盖、冷切去重与回执边界、完整备份/守护/回退和首次生产切换。
- 飞书完整任务→Worker→回复的真实证据仍缺；不再要求用户重复发消息，不虚记通过。
- 完整业务工具分支、后台/子任务和崩溃恢复矩阵。四条新增通过不等于全部 Goal 完成。

## 业务管理 v2（2026-11-02）

- 增加 `GET search`、`threads` / `threads/:key`（before/around）、`decisions`、
  `policy/history`，以及 `POST sources/:id/labels`。SQL 错误仍是错误，不返回假空数据。
- Prompt 查询增加历史分页、内置部件及 base；UI 增加搜索、线程/研判操作、历史、
  群名称刷新、Prompt 预览和受控重启。不是完整原版视图装饰签收。
- `POST setup/check` 与人工账号更换使用同一受控重启：原 Source/Worker/子进程和
  SQL owner 先回收，再重新获取状态租约。模型工具不能选择这个人工端口。
- 账号保存新增只读预验证；失败返回400且旧版本/设置/服务不变，成功保存仍经过 CAS。
- 保留 Mac CI 在旧 head 6896a9c 的真实失败：过大请求体被拒绝后，尚未发送完的
  HTTP 请求出现 reset，客户端收不到400。现在只在原五秒期限内有界丢弃最多2 MiB
  超限尾部，超限内容不保留、不解析、不提交 SQL；原128 KiB接受上限不变。
  原400断言保留，并新增结构化错误及 paused 不变断言。

新精确 head 的跨平台 CI 另记，不引用旧 head 的绿灯作为当前通过证据。
