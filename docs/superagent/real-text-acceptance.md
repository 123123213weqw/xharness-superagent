# 真实验收进展（2026-11-02）

**尚未完成生产替换，也没有把六个原版实例全部切走。** 本次不再只做离线夹具。

## 最新执行节点（2026-11-02）

- 新工作区可由产品以固定仓库/分支完成可信 checkout；真实 C1 已运行离线 workspace
  check、独立 Rust 正向/负对照测试，并创建 [Draft PR #7](https://github.com/123123213weqw/xharness-superagent/pull/7)。
- 管理查询/历史/标签、账号预验证和 join-before-restart 已接入；不是完整原版 UI 签收。
- 单 canary 完成8对象/11000文件完整备份、冷导入、原端口隔离启动和完整原路径回退，
  原版恢复 ready；六个原版实例仍在运行。没有把隔离演练写成全量生产切换。
- 新产品全 targets 316 passed / 2 ignored（忽略不计通过），正式入口61项通过，
  workspace 严格 Clippy 和备份工具6项通过。完整 V100 workspace 仍在两个旧飞书
  UID/iptables 部署断言失败；最新发布 head 的 CI 单独验，不引用旧绿灯。
- systemd 独立单元验证了 Root SIGKILL、同 cgroup 的 setsid 子进程清理、自动重启和
  人工停止不重启；这不是活动 Worker 或远程 SSH 取消的全矩阵验收。

[本批源码绑定、失败记录和剩余门槛](../business-completion-verification.json)。
全23组仍为5 passed / 17 in_progress / 1 not_run；不把局部通过当完整 Goal 完成。

## 已实测

- [GitHub #2](https://github.com/123123213weqw/xharness-superagent/issues/2)：真实 Issue 和后续真实评论，均经正式入口、DeepSeek、授权/Ticket、Native Worker、实际文件/读取/校验、回执发送及 GitHub 回读。后续评论在同一 Ticket 建立第二个 assignment；重启后仍为一个 Ticket、四份 sent 回执、零待办。
- 两个文件实际 UTF-8/emoji 字节和 SHA-256 已独立校验；不是仅相信模型自报。
- 现有 DeepSeek 与 GLM Coding 的 **Messages** 实际工具调用及 thinking/signature 回传已通过；没有切换计费端点或新建密钥。GLM 这一项不是其完整 Worker 交付验收。
- 飞书真实寒暄被接收后判为 ignore，没有造工单。实测同时暴露 null-stdin 导致消费者退出，已用显式持管 stdin 修复；真实 CLI 持续 ready、取消与 join 已验证。完整飞书任务/回复结果见下面记录，不能用历史补读冒充实时成功。

## 回归与仍未通过

选定四个 crate 共47个 Cargo target 报告376 passed、9 ignored；另有进程回收串行2项和策略冲突定点3次通过。正式入口47项通过。workspace fmt/check/严格 Clippy 远程通过。不是全 workspace 绿或忽略项通过。

旧精确 head 的 CI 曾分别出现 legacy interrupt timeout、Swarm revision conflict；失败日志保留。并行产品策略同步测试也曾失败；修复了独立复现的夹具临时文件竞争、增强失败诊断后重跑通过，但不编造那次缺失 health detail 的根因。最新 CI 必须按准确 head 单独看。

## 仍不能省略的上线门槛

1. Worker 真实 clone/build/push/草稿 PR/评审/远程执行，以及受限交付通道；没有放宽 NetworkDeny 来凑成功。
2. 必要管理 API、人工停止/重试/配置入口及面板接线。
3. 现网团队 Prompt 等部署覆盖、冷切去重/回复边界、完整备份和回退、监督进程演练。
4. 飞书完整任务/回复签收和准确发布 head 的跨平台 CI。

[完整证据和边界](../live-text-acceptance-verification.json)。聊天/账号配置/生产数据库备份未上传公开仓库。
