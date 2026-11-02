# 正式入口的策略同步

> 历史里程碑：下文“尚未接通”的范围是本批当时边界；后续飞书文本事件/历史/读取/
> 回复已接入正式入口，当前状态见[飞书文本接线](business-feishu-live.md)。图片与完整
> Source 矩阵仍未验收，不能把后续接线倒算为本批已通过。

`orchestrator_policy_sync` 现已接到正式二进制的实际 Main → Client/SQL owner → bot CLI
→ 策略保存路径，不是只注册名称或固定返回报错。主角色14个、Worker6个应用工具的
**角色注册名称已齐**；完整工具分支及完整业务对齐仍未完成，框架 `skill` 另计。

## 本批业务

- 正式组合注册飞书 kind 的配置、策略、scope 和实例信息；按原版把通用 `admission`
  与严格 kind schema 分开验证再保留，未放宽飞书字段/身份校验。实例按已安装 kind
  排序，未知来源的原配置仍保留。这里的 kind 安装不代表事件/历史循环已经接好。
- 启动只读验证 bot CLI，排除自身 worker shim。飞书是可选来源：机器人配置坏时记录
  `feishuSetup` 错误，绝不回退用户身份，也不阻止健康的 GitHub 执行与回复。
- Main 同步实际调用 bot 的成员列表端点；拒绝不完整数据。保留原成员顺序、改名、
  移除离群成员并追加新成员，返回按 ID 差分的展示名字。工具不能切换 bot CLI。
- IO 前捕获 **policy 块 revision**。并发策略保存（即使只改名字）拒绝旧响应；其他块
  保存会保留，来源在 IO 中关闭也不凭空变成策略冲突。已准入操作使用产品 lifetime，
  不是调用工具的取消 token；退出时确实取消并 join CLI/子进程树。
- 复用现有规范化、SQL 原子事务、历史与授权 effects 保存，来源记为 `sync`。
  无变化不新增版本；通知按 `settings:policy` 合并等待队列，但投递身份包含正文哈希，
  避免第一次通知永久去重掉之后的不同保存。
- `sync:feishu` 记录真实 ready/error。失败从60秒退避、翻倍至1小时，成功清除重试；
  睡眠重试不阻塞产品 idle。当前验证包含真实失败/恢复/关闭及退避状态算法，
  **没有冒充已经跑过整套实际计时到期、并发同步和所有崩溃切点**。

**注意：原版 policyVersion 包括整个来源策略，名字和 syncChat 也在其中。**
因此仅改名仍改变 policyVersion / policyChanged，不应优化成“名字不影响版本”。
这不意味着一定撤销原 Mandate：具体授权变化仍由原 policyEffects/revalidate 决定。
本批保持原核心哈希不变；先前私有计划中相反的推测不作为实现要求。

## 验证及实际修复

新增7条正式入口测试（其中两条各包含两个变体），总37条。
外部模型 HTTP/SSE、gh/lark 服务是明确离线协议夹具，但产品二进制、Native、业务
工具、实际受管 CLI、SQL、通知、关闭/重开均为真实路径。可选机器人失败场景还实际
启动 GitHub Worker，通过 CodingToolBundle 写文件并经 Outbox 回复。
它们**不是“飞书事件 → Worker → 飞书回复”验收，也不是线上服务联调**。

新场景复现并修复了两项产品 owner 接线问题：

1. 策略保存重评候选与通知并发准备同一 Main actor 时，后续请求可能停住。现在只将
   Main 准入/通知准备串行，未串行化模型执行、增设聊天历史或修改通用 Runtime。
2. 停止时两个被动 Main 观察者中，一个排队输入可能从未开始，不会有独立 turn result。
   Main 观察者随 owner 停止退出，实际 actor、工具和进程仍由 Native.shutdown 取消/join；
   不把观察者退出当执行成功。Worker 的领域关闭/恢复逻辑未改。

完整 workspace fmt/check/严格 Clippy 与所列10个crate回归在独立 WZU_Server 目录通过，
不是完整 workspace 测试验收。固定216个参考文件、11步/86路径受控导入及10条审计
guard 保持；20份角色工具 metadata 验证通过。没有新增依赖、Cargo.lock 或内核导入。
原有 GitHub 来源关闭断言从数组位置改为准确按 id 查找，是因为飞书排序在 GitHub 前，
仍严格断言同一个 GitHub 来源关闭；工具计数也精确随新增处理器更新，没有弱化语义。

完整成功/失败记录、源指纹及先前 head 的 CI 边界见
[验证记录](../business-policy-sync-verification.json)。此前63bdb07 Linux/macOS CI两项成功，
仅证明此前飞书基础快照，不提前算作本批精确 head 的 CI。

## 下一步仍在原 Goal

- 飞书实际事件流、历史/订阅轮询、群名、正文/上下文/链接/图片和 Native 附件。
- 同一 Root 的来源生命周期、健康、Prompt/Worker 环境及 Outbox 路由。
- 正式飞书事件 → 授权 → Ticket → 实际 Worker → 飞书回复，及停止/重启/失败/恢复。
- 实际重试到期、并发同步与事务崩溃切点；全部工具分支/PR-review、API/UI、后台任务、
  并发/停止/恢复矩阵及部署/备份/回退。

23组业务清单仍为5组离线通过、10组部分推进、8组未运行；本批不新增完整组签收，
不把注册名称齐全当作功能全部完成。Goal 继续活动，已取消的全域 JS/UTF-16 不重启。
