//! Offline causal probes for REGRESSION-INTERRUPT-001. They use the unchanged
//! BasicHost legacy driver and actual LoopAgentRuntime/Core, never a mocked
//! cancellation acknowledgement. Passing finite probes alone cannot close the
//! original unexplained timeout. No product/runtime implementation is altered.
use async_trait::async_trait;
use futures::{poll, stream};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    time::Duration,
};
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;
use xharness_api::{ApiBackend, RpcId, RpcMethod, RpcResult};
use xharness_core::{
    FinishReason, IdentityContextPolicy, LoopCommand, LoopControlError, LoopEvent, LoopResult,
    ModelProvider, ProviderError, ProviderEvent, ProviderRequest, ProviderStream,
};
use xharness_host::{
    AgentRuntime, AgentRuntimeError, AgentTurnRequest, BasicHost, HostConfig, LoopAgentRuntime,
    ModelDescriptor, ModelRoute, NoTools, RunningTurn,
};

#[derive(Default)]
struct Trace {
    entries: Mutex<VecDeque<&'static str>>,
}
impl Trace {
    fn add(&self, phase: &'static str) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() == 96 {
            entries.pop_front();
        }
        entries.push_back(phase);
    }
    fn dump(&self) -> Vec<&'static str> {
        self.entries.lock().unwrap().iter().copied().collect()
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Boundary {
    StartReturned,
    EndOfEvents,
    ResultReturned,
}
struct Fence {
    boundary: Boundary,
    fired: AtomicBool,
    arrived: watch::Sender<bool>,
    release: Semaphore,
}
impl Fence {
    fn new(boundary: Boundary) -> Self {
        Self {
            boundary,
            fired: AtomicBool::new(false),
            arrived: watch::channel(false).0,
            release: Semaphore::new(0),
        }
    }
    async fn hit(&self, boundary: Boundary) {
        if self.boundary == boundary && !self.fired.swap(true, Ordering::SeqCst) {
            self.arrived.send_replace(true);
            self.release.acquire().await.unwrap().forget();
        }
    }
    async fn wait(&self) {
        let mut arrived = self.arrived.subscribe();
        while !*arrived.borrow_and_update() {
            arrived.changed().await.unwrap();
        }
    }
}
struct TextProvider;
#[async_trait]
impl ModelProvider for TextProvider {
    async fn stream(
        &self,
        _request: ProviderRequest,
        _cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("offline control probe".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            }),
        ])))
    }
}
struct TrackedRuntime {
    inner: LoopAgentRuntime,
    trace: Arc<Trace>,
    fence: Option<Arc<Fence>>,
}
#[async_trait]
impl AgentRuntime for TrackedRuntime {
    fn has_available_route(&self) -> bool {
        self.inner.has_available_route()
    }
    fn can_route(&self, route: &ModelRoute) -> bool {
        self.inner.can_route(route)
    }
    fn model_catalog(&self) -> Vec<ModelDescriptor> {
        self.inner.model_catalog()
    }
    async fn start_turn(
        &self,
        request: AgentTurnRequest,
    ) -> Result<Box<dyn RunningTurn>, AgentRuntimeError> {
        self.trace.add("start.begin");
        let run = self.inner.start_turn(request).await?;
        self.trace.add("start.inner_returned");
        if let Some(fence) = &self.fence {
            fence.hit(Boundary::StartReturned).await;
        }
        self.trace.add("start.returned");
        Ok(Box::new(TrackedRun {
            inner: run,
            trace: Arc::clone(&self.trace),
            fence: self.fence.clone(),
        }))
    }
}
struct TrackedRun {
    inner: Box<dyn RunningTurn>,
    trace: Arc<Trace>,
    fence: Option<Arc<Fence>>,
}
#[async_trait]
impl RunningTurn for TrackedRun {
    async fn next_event(&mut self) -> Option<LoopEvent> {
        let event = self.inner.next_event().await;
        if event.is_none() {
            self.trace.add("event.eof");
            if let Some(fence) = &self.fence {
                fence.hit(Boundary::EndOfEvents).await;
            }
        }
        event
    }
    async fn send(&mut self, command: LoopCommand) -> Result<(), LoopControlError> {
        self.trace.add("control.begin");
        let result = self.inner.send(command).await;
        self.trace.add(if result.is_ok() {
            "control.accepted"
        } else {
            "control.closed_or_error"
        });
        result
    }
    async fn result(&mut self) -> LoopResult {
        self.trace.add("result.begin");
        let result = self.inner.result().await;
        self.trace.add("result.inner_returned");
        if let Some(fence) = &self.fence {
            fence.hit(Boundary::ResultReturned).await;
        }
        self.trace.add("result.returned");
        result
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    host: Arc<BasicHost>,
    root: PathBuf,
    trace: Arc<Trace>,
    next_rpc: AtomicU64,
}
impl Fixture {
    fn new(fence: Option<Arc<Fence>>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "superagent-legacy-control-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).unwrap();
        let mut config = HostConfig::new(&root);
        // Isolate fixture-owned files, without changing process HOME or globals.
        config.home = root.clone();
        config.provider_id = "offline-control".into();
        config.model_id = "offline-control".into();
        let trace = Arc::new(Trace::default());
        let runtime = TrackedRuntime {
            inner: LoopAgentRuntime::new(
                "offline-control",
                "offline-control",
                Some(Arc::new(TextProvider)),
                Arc::new(NoTools),
                Arc::new(IdentityContextPolicy),
            ),
            trace: Arc::clone(&trace),
            fence,
        };
        let host = BasicHost::with_agent_runtime(config, Arc::new(runtime));
        Self {
            host,
            root,
            trace,
            next_rpc: AtomicU64::new(0),
        }
    }
    async fn call(&self, method: RpcMethod, payload: Value) -> Value {
        let next = self.next_rpc.fetch_add(1, Ordering::SeqCst);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            self.host.call(
                RpcId::new(format!("offline-{next}")),
                method,
                payload,
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} timed out; control trace: {:?}", self.trace.dump()));
        match result {
            RpcResult::Success { value: Some(value) } => value,
            other => panic!("{method} failed: {other:?}; trace: {:?}", self.trace.dump()),
        }
    }
    async fn family(&self) -> (String, String) {
        let parent = self.call(RpcMethod::SessionCreate, json!({})).await["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        let child = self
            .call(RpcMethod::SessionFork, json!({"sessionId":parent}))
            .await["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        (parent, child)
    }
    async fn prompt(&self, parent: &str, child: &str) {
        self.call(RpcMethod::SubagentPrompt, json!({"parentSessionId":parent,"childSessionId":child,"mode":"continuable","content":[{"type":"text","text":"offline followup"}]})).await;
    }
    async fn interrupt(&self, parent: &str, child: &str) {
        let result = self
            .call(
                RpcMethod::SubagentInterrupt,
                json!({"parentSessionId":parent,"childSessionId":child,"mode":"continuable"}),
            )
            .await;
        assert_eq!(result["accepted"], true);
    }
    async fn idle(&self, child: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = self.host.snapshot().await;
                let session = snapshot["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["sessionId"] == child)
                    .unwrap();
                if session["running"] == false {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("idle timed out; trace: {:?}", self.trace.dump()));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Every test waits for the real child driver to become idle first.
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
async fn at_boundary(boundary: Boundary) {
    let fence = Arc::new(Fence::new(boundary));
    let fx = Fixture::new(Some(Arc::clone(&fence)));
    let (parent, child) = fx.family().await;
    fx.prompt(&parent, &child).await;
    tokio::time::timeout(Duration::from_secs(10), fence.wait())
        .await
        .unwrap();
    let mut interrupt = Box::pin(fx.interrupt(&parent, &child));
    // Poll the real control RPC into its wait while the selected factory or
    // terminal handoff is held, then release that exact boundary. No sleeps.
    assert!(matches!(poll!(interrupt.as_mut()), Poll::Pending));
    fence.release.add_permits(1);
    interrupt.await;
    fx.idle(&child).await;
    assert!(fx.trace.dump().contains(&"result.returned"));
}
#[tokio::test]
async fn interrupt_orders_with_real_loop_start_handoff() {
    at_boundary(Boundary::StartReturned).await;
}
#[tokio::test]
async fn interrupt_settles_when_core_is_closed_before_host_eof() {
    at_boundary(Boundary::EndOfEvents).await;
}
#[tokio::test]
async fn interrupt_settles_across_result_to_idle_handoff() {
    at_boundary(Boundary::ResultReturned).await;
}
async fn stress() {
    let fx = Fixture::new(None);
    for _ in 0..512 {
        let (parent, child) = fx.family().await;
        fx.prompt(&parent, &child).await;
        fx.interrupt(&parent, &child).await;
        fx.idle(&child).await;
    }
}
#[tokio::test]
async fn current_thread_real_subagent_prompt_interrupt_512_boundaries() {
    stress().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multithread_real_subagent_prompt_interrupt_512_boundaries() {
    stress().await;
}
