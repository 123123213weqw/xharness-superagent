# GitHub Source 接入进展

> 本页保留 Source 库接线批次的历史边界。后续正式二进制/编码工具工厂现已接入，
> 最新路径见 [正式入口](business-entry.md)，不能用下面历史缺口覆盖最新验收记录。

当前是**真实 Source 和 Native 产品库的装配**；正式产品启动二进制尚未交付，
不能据此签收完整 FLOW-01 或真实 GitHub 服务验收。

## 已接入

- `sources::github`：来源设置/观察范围、账号与仓库身份、REST 数据解析及稳定 Trigger ID。
  保留原版线程编号、actor/human、mention 边界、删除账号 ghost、inline diff 和 review 状态。
- `Cli`：配置的 agent gh 命令以 argv 执行，不拼 shell；API 限定在部署仓库下。
  只读检查账号和 push 权限、采用 GitHub 返回的 canonical 仓库名；不授予权限或创建凭据。
  复用现有 `xharness-process` 的超时、进程组、输出上限、取消及等待结算。
- `application::github::Source`：通知、仓库 issue/PR/评论流、已订阅线程和工单 PR，
  向同一个 Application/Engine 接收事件，而不是向旧 gh-host 另起一套执行循环。
- Source KV 写在同一 SQLite owner：5秒重叠游标、每线程游标、失败重试、流启用基线、
  ETag、默认分支/PR CI 基线。读取失败不会提前推进该线程游标；通知失败不回滚其他流。
  通知失败24小时后放弃，订阅线程使用14天活动窗口；单次失败按阈值抑制健康噪声。
- 同轮 PR 读取缓存成功与失败；已正确绑定的 PR 不反复更新工单；agent 在本仓库、
  工单命名分支上的 PR 可以绑定，关闭/合并经核心命令结算并停止对应 Worker。
- `channel::send`：回执 HTML marker 幂等、账号核对、inline 根讨论、原评论删除后
  回退顶层回复；404保留重试，410或403 locked才视为 gone。

没有 auto-merge，没有启用 full-access，也没有实际调用付费模型或写入线上仓库。
这批只新增对**已存在的工作区 crate** `xharness-process` 的直接依赖；未新增外部
Rust 包或改变其锁定版本。Cargo.lock 的一行本地依赖边以独立精确 overlay 校验；
旧11步/86路径导入链和216个固定参考源码均保留不变。

## 运行与所有权

调用方先完成配置、账号/仓库只读验证并建立 `Product`，驱动其本地 Runner 后启动。
`github::ports` 安装实际 scope、Source enabled 与 channel 发送端口；enabled 读取
Product 的同一 Settings 快照，而不是始终返回 true 的测试开关。
`Source::run(stop)` 必须由产品启动 owner 持有并在退出时取消、等待；它不自行 detach。
关闭顺序需要先停止 Source 接单，再关闭 Product。`poll_once` 是同一个轮询实现的
显式单轮边界，供启动诊断和可重复协议测试使用，不是另一套产品模拟器。

## 验证边界

1. Node 执行未修改参考的 Trigger/channel 函数，生成57个普通产品协议/账号元数据合法性样本。
   Rust 比较事件身份、完整消息字段、mention/地址与投递动作/结果；不是全域 JS 比较。
2. 实际临时 gh 可执行文件验证 argv、只读身份/仓库检查、响应头、HTTP/JSON错误、
   Unicode 正文和取消等待。没有连接真实 GitHub。
3. Source → 真实策略/SQL/队列 → Native 主/Worker → 实际文件工具 → marker 回复的
   离线组合场景；还覆盖未授权/机器人、重试恢复、健康阈值、ETag、独立失败、取消、
   CI 基线与恢复、PR 绑定/稳定重读/失败缓存/关闭取消。CLI协议和模型仍为明确夹具。

最新命令、统计和源码绑定见 [验证记录](../business-github-verification.json)。
所有 Rust 编译与测试仅在 WZU_Server 的独立目录或 CI 执行。

## 仍需完成

- 正式入口、配置/启动流程及真正编码工具工厂，把上述 Source 路径接到可运行产品，
  不把分别通过的 CLI 测试和库级模型夹具拼称完整入口验收。
- 主14/Worker6工具的余下实现；`orchestrator_report(pr_number)` 的在线只读归属校验
  尚未接到工具事务，目前仍明确拒绝，不因 Source 能轮询 PR 就假装该分支可用。
- Source 的 bounded 上下文/资源读取、模型提示环境、完整 inferred 研判、管理 API/UI、
  配置变更后的受控重启、多来源组合与飞书、所有崩溃切点和部署/回退。
- 这里没有证明原版全轮询函数与新实现的全部分支差分，也没有证明跨语言旧 CI
  fingerprint/特殊排序的迁移；当前证据只覆盖列出的普通协议和业务场景。

23个全产品业务组仍未签收；这不是缩小 Goal，后续仍按完整业务范围继续。
