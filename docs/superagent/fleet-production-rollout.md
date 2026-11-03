# 六实例生产切换（2026-11-03）

本批将既有 RS 更新到 GitHub 自主探索版本，并冷切其余五个原版 DSH 实例。
这是部署完成记录，**不是原版所有业务分支已零偏差对齐的签收**。
最终机器核对见 [部署证据](../fleet-production-verification.json)。

## 当前版本与入口

统一运行代码 `5d850965dd5e39733937ed21ee7db79d12e104a9`，不可变发布
`5d850965dd5e-ebc5ea886e4f`。本批使用已验证的远端 dev-profile ELF，
没有把它描述成另行验证过的 release-profile 构建。

| 实例 | 管理端口 | 原有推理模型 | 人工运维入口 |
|---|---:|---|---|
| Runtime | 3092 | DeepSeek Flash / high | `~/bin/superagent` |
| RS | 3093 | DeepSeek Flash / high | `~/bin/superagent-main` |
| RWKV | 3094 | GLM-5.3 | `~/bin/superagent-rwkv` |
| Pi-RS | 3095 | GLM-5.3 | `~/bin/superagent-pirs` |
| Xandbox | 3096 | GLM-5.3 | `~/bin/superagent-xandbox` |
| Arena | 3097 | GLM-5.3 | `~/bin/superagent-arena` |

以上入口在 V100 上使用，支持 `status`、`start`、`stop`、`restart`、`logs [-f]`。
管理接口仍只监听 loopback；原有 GitHub/飞书账号、授权、仓库、会话范围、
模型选择及 team prompt 内容保留。不同实例没有合并状态或仓库缓存。
`webSearch` 均显式使用 `deepseek-search-api`，并复用同一份既有 DeepSeek 凭据；
这不把 GLM 的推理调用改成 DeepSeek。没有新账号、权限授予或自动合并设置变更。

每个实例由独立 user systemd unit 管理，启用开机启动、故障重启和 cgroup 清理；
既有用户 linger 已启用，未修改其他用户/系统服务。五条旧 cron 重启命令逐条禁用，
无关 cron 保持原样，避免旧、新消费者同时连接同一来源。
Git-only 网络出口复用既有独立受管 helper，不复制 RS 的仓库缓存或账号配置；
模型/飞书/搜索流量没有被改到该 Git 路由。

## 切换与数据边界

- RS 使用同 schema 的串行升级：当前状态完整快照校验后换二进制，保留4条既有工单。
- 其余实例先验证各自模型配置和原账号，再暂停、停止并回收见证过的旧进程树。
  独立 SQLite snapshot 经 cold importer 验证，无未交付输入、执行中工作、
  未结回执或待处理 effect 后，导入观察记录、来源游标、决策、配置和已结回执。
- 五个旧实例的18条历史工单及 DSH 会话**归档保留，不续跑或假装迁成 Native 会话**。
  原工作目录继续留在原位置，包括 Runtime 约25 GiB 的目录和 Xandbox 的受限文件；
  未 chmod、删除或谎称已逐文件校验这些不可读工件。
- 已校验的 legacy bundle 覆盖独立数据库、控制/配置、旧 CLI、Git 配置及匹配的
  DSH 会话；它不冒充上述全部工作树的第二份完整哈希备份。
- 原版 revision 0 的实际 team prompt 可能没有数据库记录。切换前导出实际内容，
  在新实例仍暂停且来源关闭时，通过人工管理 CAS 写入并比对内容，再启用来源。
  Arena 首轮因此被拦截，候选未激活即恢复原实例；修正操作步骤后重试通过，
  没有取消提示词校验或恢复旧数据库来掩盖问题。

新运行时在同一 OS 账号下采用明确的 Worker full-access 配置，保留
`NoNewPrivileges=yes`；没有新增提权、账号或跨实例凭据隔离承诺。
涉及 sudo/KVM 等特权工作流的能力，不能由本批普通读取探针推定。

## 验证范围

- 源码/产物与上一批407项选定 Rust 回归、严格 Clippy 和构建的指纹一致；
  12项 ignored 未计为通过。本批未冒充全 workspace/跨平台 CI 重跑。
- 六个实例逐一检查运行中 ELF 哈希、ready、GitHub/飞书来源、暂停状态、开机启动、
  模型和提示词保留；另进行跨至少一个默认 GitHub 轮询周期的稳定观察。
- 每个实例使用自己的既有账号，对其实际仓库执行分支及 literal issue search，
  并由正式 Native Worker 的预编译测试入口完成真实 CLI 集合读取。
  推理端和触发/回复端是显式 loopback fixture，没有付费模型推理或真实外部写操作。
- 飞书来源 ready **不等于本批又完成了一次真人消息→真实 Worker→真实回复验收**。
  未要求用户重复发送测试，也没有将机器人消息冒充真人输入。
- 未重新跑历史554次读取任务的性能基准；完整 PR 多轮修改及所有业务分组仍单独记录。

## 后续维护

在服务器私有 fleet registry 中，六个实例现在都是 `rust`：RS 为 canary，其他五个为
stable。公共工具是 `scripts/superagent/fleet-control.py`，不要在六份代码上分别打补丁。

1. 在线问题用 `incident --instance NAME --summary ...` 记录实例、发布和健康信息；
   原始聊天、凭据、私有提示词和启动日志不自动公开。
2. 在统一仓库补最小复现/回归，按规则同步当前源码到 V100 编译测试。
3. `stage` 固定源码指纹、通过的证据和二进制哈希；先 `plan/upgrade --instance rs`。
4. 检查真实 canary 后，以限定范围的 acceptance 显式 `promote`。
5. 其余实例逐个 `plan/upgrade`；有运行、等待或排队工作就延期，不全局重启。
   每个实例继续保留自己的状态、模型、提示词、账号和工作目录。

兼容 Rust 发布的回退也使用保留当前状态的受控升级，不覆盖旧数据库/旧游标。
**回到原版 DSH 必须先对账上线后的工单、消息/PR副作用和游标，不能直接启动旧消费者。**
本批没有启用无人值守源码更新、自动公开缺陷或自动修改生产。
