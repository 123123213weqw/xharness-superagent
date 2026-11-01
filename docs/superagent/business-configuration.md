# 业务配置与团队提示词（部分已接通）

本批新增正式主角色工具 `orchestrator_settings_save` 与 `orchestrator_prompts_save`。
应用工具现为主9/Worker6（共15个注册），仍缺主角色 Note、3个 eval、policy_sync；
注册数量不是业务完成率。飞书、完整推断、API/UI、后台/停止/恢复/部署仍在 Goal 内。

## 同一产品 owner，不开放任意 KV 写入

调用从可信主角色绑定进入原 Application Client，再调用弱绑定的 Native 配置处理器。
模型不能提交 panel/sync origin 或选择别人的会话。Worker 不注册这两个工具。
现有 identity-aware 配置值仅用于保证读改存和 policyVersion，不扩展全域 JS/UTF-16。

`settings_save` 接受原版的 block/source/value/revision：

- policy、eval、executor 与 source 独立块版本；先检查过期，错误不写历史。
- 全块验证后保存；设置 no-op 不新建版本。暂停块忽略入参 revision，自身 no-op
  同样不写历史。字段错误、未知/未安装 Source、无效账号名单均为真实错误。
- 工具不能修改执行目录、prepare 或 blockedCommands，也不能换仓库/账号 CLI。
  这些仍要求后续管理面板的受信入口；模型不能以伪造 origin 绕过。
- 角色模型在已配置的实际 Runtime registry 中校验，不调用付费模型试探、不回落
  到另一个模型。新会话采用当前保存的角色默认；已有持久/人工选择不被覆盖，
  全局默认不被改动。
- 策略保存将设置历史、受影响工单通知及重新研判意图放入同一事务；提交后才
  更新现有 policy/scope watch。真实 SQL 故障测试验证历史、权限与派发一同回滚。
- 暂停**不撤销正在运行主会话已记录的接单决定**，而是挂住新 Worker 派发和回复。
  正式测试在暂停中看到 queued 工单但没有 Worker Session/模型请求、没有发出 ack；
  恢复后才实际写文件和回复。未将“暂停”错误实现成“所有工具禁止使用”。

当前工具 origin 下，改变 setup 账号或执行环境的保存被原版 guard 拒绝，因此不会
假报“已重启”。完整 panel 保存、账号探测/重启、Policy 同步与配置 API/UI 仍未完成。

## Prompt preview、版本与 next-request

`prompts_save` 的 preview=true 只组合两角色提示词，并将结果放入当前主会话的有界
易失 page cache；不写历史，可用 `orchestrator_read prompt-preview/...` 分页。

正常保存要求 baseRevision，未提供 preview 按原版默认为 false；过期版本报冲突。
首次保存保留 defaults 版本0；与 settings 不同，保存完全相同的 Prompt 仍新增版本。
保存不会给主角色发自通知，实际下一次模型请求即绑定新文本和技能版本；重启保留。

原产品入口过去使用无技能的 Prompt channel，组件里有的技能管理器并没有接入
正式产品。本批接入**已有** scoped Skill Manager，并发布可发现的名称/描述；
Worker 正式调用 skill 得到保存的团队技能全文，不只检查内部注册表。

编译进二进制的两个固定内置 SKILL.md 安装到本产品 stateDir 的内容寻址目录，
不依赖编译机的源码路径。初始化/`--check` 允许写这些非秘密本地包资源（仍无模型
调用或外部写操作）。重复安装不覆盖已有内容；不匹配/符号链接报错，保留外部内容；
保留的安装临时文件可在独占状态目录下恢复。缓存/资源目录均不是第二份聊天存储。

skill 是框架工具，不计入主14/Worker6应用工具；没有因此开放通用 Goal、schedule、
额外模型、shell/web 权限。Prompt/技能文本不会给工单新增授权。

## 验证边界

新增4条正式入口场景：设置/模型/Source guard、Prompt版本/预览/实际技能加载、
暂停/恢复挂起 Worker 与回复、SQL 切点下的策略历史/权限/意图回滚。
另有2个本地资源安装边界组件测试；它们不是部署/完整产品验收。

外部模型仍为显式 loopback HTTP/SSE，gh 为受控可执行协议夹具；实际二进制、
OpenAI适配器、同一 SQL/Native/CodingToolBundle/Outbox 路径均运行。没有线上评论、
付费调用、新账号、生产部署或自动合并。完整证据及未验收项见
`docs/business-configuration-verification.json` 与 `conformance/business/acceptance.json`。
