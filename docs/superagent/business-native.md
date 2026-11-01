# B1：Native 产品装配进展

这是库级真实调用路径，不是可部署产品或完整业务验收。

## 当前连接

`application::native::Product` 在单个本地 SQL owner 上组合 Application、
NativeSessions、PromptRuntime、NativeDelivery 和生命周期观察者：

1. 获取状态目录租约，再打开 SQLite；已有配置和主会话身份保持不变。
2. 入站 Trigger 经过真实 Engine/SettingsPolicy；评估由持久化 assessment 派发。
3. 主会话实际调用 `orchestrator_route`；Ticket/Assignment 与启动意图进入 SQLite。
4. 显式工作区准备端口建立执行目录；Worker 使用实际 Native 会话与注入的工具工厂。
5. Worker 的 `orchestrator_report` 将报告和给主会话的 relay 原子提交。
6. 主会话调用 `orchestrator_reply`，由真实 Outbox 准备、Source 发送、回执结算。
7. 关闭拒绝新接单、取消运行中的会话，并等待派发、观察者和 Passes 结算。

模型、Sources、工作区准备和基础编码工具都必须显式注入。没有 ScriptedProvider、
假克隆、假发送、无工具运行或扩大至 full-access 的生产默认回退。
测试中的固定模型响应、文件写入工具、工作区创建和记录式 Source 都是明确夹具。

## 边界修复

- Native 模型工具是 Send 回调，业务 Store 是本地 owner；有界 Client 通道传递可信
  Rust 操作，不接收模型代码、不复制会话历史。入场后操作继续结算，丢失响应可计数。
- NativeDelivery 使用原始 request ID，旧直接 prompt API 使用哈希 ID。显式区分
  两种命名空间，保留旧 API 默认行为，不通过重复投递消息补救观察失败。
- 实际反例证明：仅在入队后订阅可能错过立即失败/结束。产品选择 `new_observed`，
  在持有真实 prepared-input 能力时、逻辑提交前安装原生 RunningTurn 观察者。
  新 Host 接口只订阅，不授权、不追加、不唤醒、不改变旧运行时默认。
  这17行作为独立产品 overlay 登记，不伪称 canonical 导入：校验器先验证
  固定 overlay 哈希、精确重建改动前文件，再执行原11步/86路径的完整哈希链。
  额外负例拒绝 overlay 内外漂移；未改固定参考源码或原受控导入记录。
- `idle()` 不把原生模型/工具准备失败当成成功；失败也保留到关闭结算。
- 参考工具 Schema 的 8 处原始 `anyOf` 是不相交 primitive/null 联合。
  仅在产品清单中等价改写成 `type: [primitive, "null"]`，保留字符串/数值约束；
  不放宽 XHarness 全局 Schema 校验。独立脚本核对四份清单的全部其余元数据。

## 已接入的工具子集

| 角色 | 当前安装 |
| --- | --- |
| Main | `orchestrator_route`、`orchestrator_reply` |
| Worker | `orchestrator_ack`、`orchestrator_report` |

工具绑定可信 Native 会话和工作区；Worker ticket ID 由绑定给出，不接受模型自选。
尚未安装的工具不作为空实现对模型宣称可用。

## 测试边界

`product_native` 的离线组合检查经过真实 SQL/权限、持久队列、模型请求、Native
循环、业务工具处理器和 Outbox；正向场景实际写入临时工作区的中文/emoji 文件。
另覆盖未授权无模型调用、ignore 不启动 Worker、批次部分拒绝、模型错误可见、
关闭取消运行 Worker，以及正常关闭后重启不重复执行/回复。

这些检查**没有**经过正式二进制、GitHub/飞书 Source 事件解析、真实服务发送或付费模型；
不是 FLOW-01 / AUTH-01 等完整业务验收，不证明崩溃中断点重放或所有恢复分支。
具体验证命令和结果见 [机器可读记录](../business-native-verification.json)。

## 下一步

优先补正式启动/配置入口和 GitHub Source，把同一条 Native 路径接到实际协议边界。
继续补全主14/Worker6应用工具及其输入约束、资源读取和推断研判；当前4工具子集
不代表这些工具的所有分支已对齐。PR 归属校验未接入时拒绝 `pr_number`，不伪造成功。
后台进程/子任务、30分钟生命周期、失败重试、崩溃恢复、动态配置/Prompt/skills、
Feishu、API/UI 与部署/回退仍是当前 Goal 的未完成工作。
