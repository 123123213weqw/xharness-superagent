# 飞书协议与受管 bot CLI 基础

> 本文保留该基础批次当时的状态；后续 kind 配置和真实 `policy_sync` 已进入正式入口，
> 当前见 [策略同步](business-policy-sync.md)。飞书事件/历史/上下文闭环仍未完成。

**这是接入飞书所需的来源基础，不是飞书产品闭环验收。** 正式入口尚未安装飞书
来源，`policy_sync` 尚未注册；主角色仍为 13/14、Worker 6/6 个应用工具。
不把已有旧 feishu-host 或这些组件测试冒充新的产品接入。

## 本批实际代码

- 配置默认值、群/私聊 scope、策略身份和重复 ID 校验；保留飞书自己的 open_id /
  app_id，不与 GitHub 账号合并身份。
- 消息归一化、稳定 trigger/thread/channel 身份、自身 bot/app 过滤、私聊/提及区别、
  提及姓名替换、正常消息时间和 AppLink。这里只处理飞书协议，不复刻全域 JS 日期语言。
- 验证真实 CLI 路径、auth status、strict-mode=bot、bot info。忽略 worker shim 与相对
  PATH 条目，保留选中别名；不自动切换用户身份、不修改账号。探测 60 秒、普通请求
  90 秒、事件 ready 45 秒超时；使用现有 ProcessRuntime，不新增 shell 或权限。
- 成员同步实际执行 bot 的 chat-members-list，拒绝 has_more/truncations 和缺失必需数据；
  保留原成员顺序、原位更新姓名、移除离群成员并追加新成员。**尚未连接策略保存事务。**
- 飞书回复使用 thread 和 receipt idempotency key；只有带代码的明确撤回/不存在才成为
  Gone，超时、未知错误和未入群仍为错误。`ok:true` 的无返回体回复可以成功，要求
  数据的成员读取仍校验整个 payload，不把缺失数据当空成员名单。
- 受管事件流保留分段中文/emoji，报告坏 JSON / warning，处理 ready 与无换行 EOF。
  自然退出是错误，不伪造 ready；取消/超时等待实际 CLI 与子进程树回收。

## 证据范围

新增 9 条来源/CLI 边界测试，不计作 9 条产品测试。它们调用真正受管子进程，但
外部 lark 服务是明确的离线可执行夹具，没有真实飞书、账号、网络或付费模型调用。
ready / setup 超时测试先启动并确认实际 PID，再推进测试时钟；回收阶段恢复真实时间。

另有 52 个原版行为样本：12 消息、8 配置、7 策略、6 scope、8 成员同步、8 回复、
2 链接和 1 提及替换。trigger/channel/Commands/sourceContext 直接导入原版。
完整 kind 导入缺少无关 SDK 的 schemastery，保留该失败；配置/策略 schema、scope
和 sync 方法仅做精确原代码隔离、类型擦除/方法包装，并登记原片段哈希，不注入 SDK 桩。
这不是完整 kind/SDK 或 Runtime 的运行证明。

新增的唯一依赖边是已经在 lock 内的 `url 2.5.8`，用于正常 AppLink 验证；没有新增
外部包、刷新版本或 checksum。许可证原文已保留。独立严格 overlay 只移除此边再
进入原有审计链，固定 216 个参考文件及 11 步/86 路径内核导入不变，原断言未放宽。

所有 Rust 验证在 WZU_Server 的独立目录完成。最终 workspace fmt/check/严格
Clippy 与所列 10 个 crate 回归通过，既有正式入口 30 条保持通过；这些仍是 GitHub
产品路径的旧场景，不代表飞书已进入 Native/Outbox。ignored 或能力门控提前返回也
不等于产品通过。完整成功/失败日志和指纹见 [验证记录](../business-feishu-verification.json)。

## 必须继续完成

1. 把来源安装/启用/就绪、实际事件流/历史/订阅轮询及 per-source 健康接到同一业务 owner，
   全部关闭时确实取消并 join，不留下后台进程。
2. 接通 trigger 锚定的正文/上下文/图片与 Native 附件，不返回伪造空内容或图片成功。
3. 通过实际来源端口实现 `policy_sync`：按原版捕获 policy block revision，处理并发冲突，
   以 sync 来源原子保存并通知，记录健康并安排不阻塞 idle 的退避重试；不是仅注册报错。
4. 正式入口验证飞书事件 → 授权 → Ticket → 实际 Worker → 回复，以及关闭/重启/失败场景。
5. 继续原 Goal 的完整工具分支、PR/Review、API/UI、后台/停止、崩溃切点和部署回退。

飞书组从“未运行”转为“正在实现”，没有计作产品通过；清单为 5 组离线通过、
10 组部分推进、8 组未运行，不是完成功能比例。完整 Goal 仍活动。
