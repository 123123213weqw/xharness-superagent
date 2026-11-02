# 真实验收进展（2026-11-02）

**尚未完成生产替换，也没有把六个原版实例全部切走。** 本次不再只做离线夹具。

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
