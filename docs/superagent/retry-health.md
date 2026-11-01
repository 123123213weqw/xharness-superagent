# 预算重试状态：参考投影与真实 JSONL 适配

固定参考 `6337fc823ce20251c8de8085948165c81aaa701e` 的 `src/app/retry-health.ts`。本组件只投影状态，不改变任何模型路由、预算、重试次数/策略/定时器或授权。

- `RetryHealth` 保留 JS Map 的插入顺序、restore 删除再插入、按 Session 的 seq watermark；先推进 watermark 再验证 data，clear 保留 watermark/settled。仅同 turn/step/provider 的非 interrupted model assistant 消息证明恢复；retry-started 不证明恢复。提供 clone-on-read、provider 标签校验与 MAX_SAFE_INTEGER 次数上限，输出不含 failure、policyKey、retryId、成本或模型内容。
- 时间使用既有核心的完整 TimeClip/ISO 格式，覆盖正向扩展年份及上限；`Number(status)` 分类与消息 UTF16 前 8192 单元边界单独比较。
- 失败消息中的 JSON 只做有界、迭代、选择性解析。完整验证 JSON 语法但只保留 root/error 的 type/code 谓词，支持 JSON.parse 的 lone surrogate escape、深层无关数组、重复属性和巨大指数；不依赖 serde_json 的 Unicode/递归深度限制，也不重写或持久化诊断文本。这不表示全产品的外来 UTF16/坏库问题已解决。
- `NativeRetryHealth` 消费现有 `LoggedEvent`，不建立第二份会话。Native assistant 不含 provider source，只与该 turn/step 内实际 request/header 连接；缺失/错 provider 或 interrupted 输出不伪造成功，不复制 prompt/tools/assistant 内容。恢复后真实 idle/disposed 必须由 Sessions owner 显式 clear，不能凭未结束日志冒充活跃 worker。

## 验证入口

所有 Rust 仅在同步含未提交源码并排除敏感/构建文件后的 WZU_Server 执行：

```sh
bash scripts/superagent/remote-validate.sh retry-health-contracts
bash scripts/superagent/remote-validate.sh legacy-control-contracts
```

参考原 29 项测试及断言未改，仅把 RetryHealth 类替换为 Rust 同步 FIFO transport；另外 44 项独立差分测试直接运行原类与 Rust 类。桥接器只传递事件/方法，Rust 进程持有全部 watermark/chain/classification，JS 不计算预期决策。桥接二进制要求显式 conformance feature，不在默认产品入口中。

3 项 Rust 投影测试及 1 项新增真实 NativeSessions 测试：明确离线 provider 返回可重试预算诊断，Core 自己写入 JSONL retry/retry-started/request/header/assistant；重试开始不清除等待，实际同 provider assistant 在 turn/end 之前清除。首次新测试未显式配置 Setup，被既有 gate 拒绝；随后只为 fixture restore + set_configured(true)，未修改/绕过生产门禁。

## 仍未完成

这里不是完整 Sessions 安装：live managed Agent 事件订阅/idle/dispose owner、models HealthEntry 与 UI、生产 budget retry 策略、角色模型/Prompt revision/skills、完整 AgentDelivery 回调/持久化故障顺序及剩余工具/工作目录/Root/Source/API/UI/部署仍待组合。没有付费模型、真实 Source 写入或生产变更；最终 96 条 Rubric 保持 not_run。

## 原中断超时仍开放

`REGRESSION-INTERRUPT-001` 首次全量的 `subagent.interrupt` 10 秒超时尚无因果解释。新增 3 个真实 Loop 的 factory-start/EOF/result→idle 屏障测试，加上单线程和双线程各 512 次实际 SubagentPrompt→Interrupt→Idle，不替换 cancel acknowledgement；fixture HOME 隔离，不修改进程 HOME/全局账户。有限探测通过不等于原始全 RPC fixture 已定位、历史失败或无害 flake；不提高超时、不跳过测试、不把重试绿色当作闭环。
