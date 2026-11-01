# 真实唤醒投递原语与受控上游补丁

固定上游 `4ab403e0bb671b69fe56c5355bcf10dad7010c0b`，Runtime基线仍是`2147ff270fe55c2c739e5063a5bfacefd01dc38a`；没有整体同步master或移动基线。canonical diff解压后SHA256为`9704604066bc132d6c0c1737f5d6acfb9c5946f649ae87b1cf180bb6698033b2`。

`conformance/upstream/noninterrupting-input.source.patch.gz` 保留原始patch字节，包括必要的空context行；不是通过trim改变源diff。`noninterrupting-input.sync.json` 记录11路径的canonical before/after及实际Runtime before/after；对已干净的导入文件做Git三方移植，新模块/测试逐字节一致。没有引入master其它功能（包括ScopedControl）。`node conformance/verify-upstream.mjs`校验当前目标与审计记录匹配，**不代表语义或发布批准**。

## 新的显式能力

- `DurableAgentHandle.follow_step`：nearest-step+wake，不中断当前模型；在actor边界读取当前turn实际取消信号，已取消则转next-turn。不是先读外部status再决定append/wake。
- `AgentRuntime.admit_follow_step/admit_followup`：先安装input-scoped subscriber，再准入并唤醒。前者调用非中断step能力，后者是唤醒ordinary followup；旧`admit_turn`仍queue-only，旧steer仍中断式。unsupported runtime返回明确错误，不回退成no-op或错误模式。
- Session结构和事件schema保持原样，没有必填Rust字段。严格versioned transport metadata承载显式调度意图与原始metadata；来源和调度分开，`is_user_authored`不改。旧入口剥离复制的有效调度标记，replacement保留既有意图，remove随输入一起去掉。foreign/corrupt旧数据命名空间碰撞并未验收。
- 内部waking step只在已开放的自动门禁可运行；closed gate不被内部输入重开。真实human仍经原有resume+claim。没有假Goal、空prompt或第二Inbox/聊天循环。

## 实际产品owned接口

`NativeSessions.followup`对应参考queue模式的ordinary wake；`steer`对应非中断nearest-step wake；`inject_notice`复用既有input-free准备+passive inject，不wake。低层`queue`保留admission-only用途，不把它冒充followup。

`holds`取current Inbox、实际消费和live inject seen。native user/message无source字段，因此按时间把消费ID关联到最近先前的Inbox provenance；有效source从transport envelope提取，不根据ID前缀猜人类或生产者。`remove_prompt`经过Runtime删除pending observer缓存；直接在live owner背后改JSONL不是此接口承诺。重复pending投递可重新flush/订阅/唤醒恢复，但不转换passive notice，不让旧user RPC标签替内部retry重开paused gate。

## 验证与未决

上游9 actor/Loop/barrier、3元数据、2unsupported测试；实际Native新增8项使总计30，owned组件119。新模块的8项真实远程编译器override反例均E0453，恢复后production Clippy通过；34架构变异通过。上游affected5包source-exact回归、Runtime全workspace与AgentDelivery39/Passes23均另有日志。全部Rust只在WZU_Server。

这些有限测试不等于完整Facade或产品：authorize/accepted/finalflush/abort与native async/durability顺序、真正文件系统故障、live Prompt/model/policy/skills/预算状态、Core审批/真实human-request scope、stale Assignment/turn取消、tools/jobs/children/Root/Source/API/UI仍待实现验证。Inbox来源测试不证明所有授权；旧reader对内部step保守停放也不等于完整回退进度一致。所有最终Rubric仍not_run。

本轮首次全量出现既有基线测试的`subagent.interrupt`10秒超时。未改断言/超时/源码，单项50次与整组24项复验通过，但原因尚未定位；`REGRESSION-INTERRUPT-001`开放，不能用重试通过或相关旧源码未变证明没有回归。首次失败日志保留在本轮证据中。
