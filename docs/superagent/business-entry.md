# 正式入口：第一条业务闭环

`xharness-superagent` 现有独立可执行入口；这是**未完成的产品开发版**，不是完整
SuperAgent 替代品。旧 gh-host/feishu-host 不参与这条路径。

## 已有路径

配置文件 → 独占状态目录 → 同一 SQLite/Driver/Passes/NativeSessions → 配置的 gh
账号与仓库只读验证 → GitHub Source → 策略/研判 → 主角色 route → Ticket/工作区
准备 → Worker 的实际 CodingToolBundle → report → 主角色 reply → marker Outbox。

提供 `--check`、`--once` 和持续运行三种模式。默认不开隐式账号、模型或高权限；
缺配置、环境凭据、注册模型或仓库权限时返回错误，不选择另一个模型兜底。

## 配置与运行

模板是 `config/superagent.example.json`，全部路径、网关、模型与仓库均为占位符。
生产二进制不含测试模型/gh 夹具。它使用现有 OpenAI 兼容适配器，支持显式选择
`chat_completions` / `responses`，并非内置 DeepSeek 账号或新 Node/SDK 服务。

- `stateDir` 必须为绝对路径，并且只能有一个活动 owner。
- `mainDirectory` 是主会话目录，省略时为启动 cwd。
- `models` 为明确注册的模型；凭据只用 `apiKeyEnv` 引用，不写进 JSON。
  变量名必须能被现有子进程凭据过滤器识别，例如 `SUPERAGENT_API_KEY`。
- `defaultModel` 是默认路由。SQLite 已保存的 `eval.model` / `executor.model`
  优先；保存了不存在的模型会报错，不静默换用 defaultModel。
- **`settings` 仅用于首次初始化**；重启不覆盖数据库设置。目前配置 API/UI 尚未
  完成，所以这不是热加载配置界面，修改已有数据库配置的正式管理流程仍是缺口。
- GitHub 必须配置 agent 的 gh 命令和仓库，使用该命令已有授权。`--check` 只读取
  账号、canonical 仓库与 push 权限，不发送模型请求/评论或授予权限；它会打开/
  初始化本地状态，并不是文件系统 dry-run。它不验证模型网络、服务可用性或工具
  能力。目前即使 Source 关闭也需完成此设置。
- 模板关闭 Source、使用 observe 模式且授权名单为空。需要主动接单时必须显式
  配置 Source、`eval.mode` 和 policy；程序不会替用户扩大名单或授权。
- `executor.prepare` 是**用户配置**的准备命令，在工作区父目录执行，最长600秒；
  为空只创建目录，不会暗中 clone 仓库。命令可使用 `DSH_SUPERAGENT_DIR/NAME/TICKET`
  和 GitHub/飞书的已解析非秘密变量（见[飞书接线](business-feishu-live.md)）。失败终止该 assignment 并报告 failed；保留非空
  半成品，不自动把半成品当作准备成功重试。该受信配置命令不是 Worker 模型输出。

所有 Rust 构建/检查/测试只能在同步源码后的 WZU_Server 独立目录或 CI 执行。
下面是**运行已构建二进制**的用法，不是在本机构建：

```sh
/path/to/xharness-superagent --config /path/to/config.json --check
/path/to/xharness-superagent --config /path/to/config.json --once
/path/to/xharness-superagent --config /path/to/config.json
```

持续模式用 Ctrl-C 停止；Source、产品队列、Native 会话和 managed jobs 都由入口
关闭/等待。单轮模式也能中断正在等首个 SSE 字节的模型请求。成功输出 JSON；
`idle` 只表示本轮已静止，不表示每张 Ticket 成功，失败报告仍保存在业务状态中。
`waiting` 表示仍有持久化待办，不把重试等待算成功执行。SIGTERM/服务部署另待验收。

## 工具与边界

入口首批是 route/reply/ack/report 4个应用工具。当前主14/Worker6的角色应用工具
名称均已注册，框架 skill 另计；这不表示所有业务分支验收。控制工具历史里程碑见
[控制接线](business-controls.md)，最新部分验收见业务清单。
Worker 另装实际编码工具：本机平台对应的 bash/pwsh、read/write/edit、
glob/grep 和 managed job 的 output/list/kill。没有假文件写入处理器。

- `workerAccess` 默认 `workspace-write`：保持 WorkspaceWrite + NetworkDeny。
  只有操作者在启动配置显式设置 `"workerAccess": "danger-full-access"` 才启用
  Worker 原生 FullAccess（网络允许、工作区外文件访问、无交互审批）。该选择记录在
  同一会话 JSONL，既作用于新 Worker，也在旧 Worker 再绑定前同步；主角色工具和
  通用 Host/Platform 默认不变。此模式使用服务自己的操作系统账号，不产生 root 权限。
  改回 `workspace-write` 后重启，会在 Worker 再绑定时收紧权限。
  账号仍需现有已授权 CLI 配置；联网本身不会产生 GitHub/飞书凭据。
- 通用 CodingToolBundle 的交互式审批默认值不变；**这个产品专属投影**把已获
  Ticket mandate 的 write/edit/bash/pwsh 交给实时 TicketAuthority guard，避免
  无管理 UI 时永远等待交互审批。每次调用检查同一 SQL owner 的活动 assignment、
  暂停/结束和当前授权；不是无条件 ApproveAll。文件系统和进程按操作者选择的平台策略执行；full-access 不具有工作区沙箱隔离。
- 默认不暴露会绕过进程网络沙箱的 web_fetch/web_search；显式 full-access 才提供。
  两种模式均不泄漏 Host 的 Goal/调度工具，不改变 Ticket/assignment/角色授权。
- 编码子进程非秘密环境是显式 opt-in，拒绝凭据、PATH、XHARNESS 控制变量覆盖；
  通用调用方默认仍为空，凭据过滤及 managed PATH 不变。

真实适配器联调还修复了一处窄依赖问题：只有 Text blocks 的 Native 输入无需附件
resolver；File/Image 仍必须有真实 resolver。保留 blocks 原文，不降级为假附件成功。
这不是恢复 JS/UTF-16 语言级复刻。

## 验收证据与剩余范围

整链路测试真正启动 `CARGO_BIN_EXE_xharness-superagent`，经过现有 OpenAI HTTP/SSE
适配器、实际 gh 子进程、Native JSONL、真实 write 工具和 Source 回复；只有模型
服务及 GitHub 服务边界是显式离线协议夹具。它验证中文/emoji文件、越界写拒绝、
ready/ended 状态、ack/result回执、角色工具隔离、重复事件/重启不重复执行，以及
未授权/机器人/ignore不执行、准备失败、配置失败、独占锁与中断关闭。

它**不是**真实 GitHub/付费模型验收，也没有覆盖全部 Source、inferred 研判、剩余
工具/资源读取、飞书图片/附件、配置 API/UI、所有崩溃切点/后台任务、部署/备份/回退。
完整 Goal 不缩减。实际命令、范围和日志绑定见 `docs/business-entry-verification.json`。

## 可选飞书来源与成员同步

在原配置的 `settings.sources` 和 `settings.policy.sources` 中分别**追加**以下部分，
不要删除全局仍必需的 GitHub 仓库/账号配置；CLI 必须是真正的 bot-only 配置。

```json
{
  "sources": {
    "feishu": { "enabled": true, "config": { "cli": "/absolute/path/to/bot-lark" } }
  },
  "policy": {
    "sources": {
      "feishu": { "allowed": [], "syncChat": "oc_replaceWithYourChatId" }
    }
  }
}
```

这是 `settings` 内合并片段，不是独立可启动的完整配置。`--check` 只读探测账号，
不读取群成员；正常启动自动同步配置的群，Main 也可调用 `orchestrator_policy_sync`。
来源关闭、无同步群和无同步能力分别按原版报错；bot 缺失/坏配置保持可观察错误，
不切换用户身份、不让工具改账号。详细边界见 [策略同步](business-policy-sync.md)。
当前已有正式飞书文本事件/历史/上下文/回复循环，配置群范围、名单及模式的完整
片段和验证边界见[飞书文本正式入口](business-feishu-live.md)。图片附件及完整线上
服务联调仍未完成；上面的同步片段本身并不配置群范围或主动接单模式。

## Full-access 账号绑定

`workerAccess` 是启动配置，不允许模型或来源事件修改。它不创建 token、修改仓库
授权、开启自动合并或改变其他实例。部署时可以在**该服务**的 PATH 前置操作者已有的
`gh` / `lark-cli` 包装器，设置该账号的 `GIT_CONFIG_GLOBAL` 和
`GIT_TERMINAL_PROMPT=0`；包装器应移除自己的 PATH 项后再调用真实 CLI，避免递归。
不要把模型 API key、GitHub token 写进这份配置或 Prompt。子进程仍清除凭据环境变量、
保留取消/超时/后台任务回收；full-access 并不阻止进程读取同一系统账号有权读取的文件。
网络权限开放也不代表批量 Git 传输、已有 PR 续改等业务流程已经验收。
