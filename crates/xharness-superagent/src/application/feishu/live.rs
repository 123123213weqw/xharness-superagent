//! Feishu live/history/name loops on the existing SQL owner. There is no second
//! message store; all accepted and filtered Triggers go through Application.
use super::*;
use crate::sources::feishu::{consumer, read, trigger, Transport};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use xharness_superagent_core::{
    engine::time,
    model::{live_ticket, thread_key, Thread, Trigger},
};
pub type Health = crate::application::github::Health;
const HORIZON: i64 = 14 * 24 * 3_600_000;
struct Scan<'a> {
    chat: &'a str,
    direct: bool,
    since: &'a str,
    until: &'a str,
    stop: CancellationToken,
    skipped: &'a mut usize,
}
#[derive(Clone)]
pub struct Source {
    application: Application,
    cli: Option<Arc<Cli>>,
    health: Rc<RefCell<BTreeMap<String, Health>>>,
    problem: Option<String>,
}
impl Source {
    pub(super) fn new(
        application: Application,
        cli: Option<Arc<Cli>>,
        problem: Option<String>,
    ) -> Self {
        Self {
            application,
            cli,
            problem,
            health: Rc::new(RefCell::new(
                ["feishu", "history"]
                    .map(|k| {
                        (
                            k.into(),
                            Health {
                                state: "starting".into(),
                                detail: String::new(),
                                code: None,
                                at: None,
                            },
                        )
                    })
                    .into(),
            )),
        }
    }
    pub fn health(&self) -> BTreeMap<String, Health> {
        self.health.borrow().clone()
    }
    fn report(&self, id: &str, state: &str, detail: String, code: Option<&str>, dated: bool) {
        self.health.borrow_mut().insert(
            id.into(),
            Health {
                state: state.into(),
                detail,
                code: code.map(str::to_owned),
                at: dated.then(|| self.now()),
            },
        );
    }
    fn now(&self) -> String {
        (self.application.state.context.clock)()
    }
    fn config(&self) -> Result<Option<Config>> {
        Ok(Config::resolve(&self.application.state.store.settings()?))
    }
    fn cli(&self) -> Result<Arc<Cli>> {
        self.cli
            .clone()
            .ok_or_else(|| Error::Message("The Feishu Source has no working bot CLI".into()))
    }
    fn available(&self) -> bool {
        let configured = self.application.state.configured.get();
        if configured && self.cli.is_some() {
            return true;
        }
        let (code, detail) = if configured {
            (
                "feishuCliUnset",
                "The Source is on, and its bot lark-cli is not set",
            )
        } else {
            ("setupPending", "Waiting for the deployment check")
        };
        if let Some(problem) = &self.problem {
            self.report("feishu", "paused", problem.clone(), None, false);
        } else {
            self.report("feishu", "paused", detail.into(), Some(code), false);
        }
        false
    }
    fn check(&self, stop: &CancellationToken) -> Result<()> {
        if stop.is_cancelled() || self.application.state.closing.get() {
            Err(Error::Message("Feishu operation cancelled".into()))
        } else {
            Ok(())
        }
    }
    fn state(&self, key: &str) -> Result<JsValue> {
        self.application
            .state
            .store
            .get_raw(key)?
            .map(|s| JsValue::parse(&s).map_err(|e| Error::Message(e.to_string())))
            .transpose()
            .map(|v| v.unwrap_or(JsValue::Object(Vec::new())))
    }
    fn put(&self, key: &str, value: &JsValue) -> Result<()> {
        self.application
            .state
            .store
            .set_raw(key, &value.stringify()?)
    }
    fn receive(&self, trigger: Trigger) -> Result<()> {
        let chat = trigger
            .meta
            .get("chatId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let direct = trigger.meta.get("direct") == Some(&json!(true));
        let threads = trigger.threads.clone();
        self.application.receive(trigger)?;
        if chat.is_empty() {
            return Ok(());
        }
        let labels = self.state("source-labels:feishu")?;
        let name = labels.get(&chat).and_then(JsValue::as_str).unwrap_or(&chat);
        let brand = self.cli()?.deployment().map_err(failure)?.brand.clone();
        let fact = if direct {
            json!({"value":{"zh":"私聊","en":"Direct message"}})
        } else {
            json!({"value":name})
        };
        for thread in threads {
            self.application.state.store.describe_thread(
                &thread_key(&thread),
                &json!({"url":protocol::channel::chat_link(&brand,&chat),"facts":{"chat":fact}}),
            )?;
        }
        Ok(())
    }
    fn live_message(&self, value: Value) -> Result<()> {
        let cli = self.cli()?;
        let bot = cli.deployment().map_err(failure)?;
        let Some(trigger) = trigger::normalize(value, &bot.bot_id, &bot.app_id).map_err(failure)?
        else {
            return Ok(());
        };
        // Valid out-of-scope events still enter Engine and retain filtered facts.
        // Only events read after the Source was disabled are dropped here.
        if self.config()?.is_none() {
            return Ok(());
        }
        let mut chats = self.state("feishu-chats")?;
        let chat = trigger
            .meta
            .get("chatId")
            .and_then(Value::as_str)
            .unwrap_or("");
        chats.set(
            chat,
            JsValue::Bool(trigger.meta.get("direct") == Some(&json!(true))),
        );
        self.put("feishu-chats", &chats)?;
        self.receive(trigger)
    }
    async fn watch(&self, stop: CancellationToken) -> Result<()> {
        if !self.available() {
            return Ok(());
        }
        let mut retry = None;
        while !stop.is_cancelled() {
            if self.config()?.is_none() {
                self.report("feishu", "paused", "Off".into(), Some("sourceOff"), false);
                pause(5000, &stop).await;
                continue;
            }
            let running = stop.child_token();
            let began = tokio::time::Instant::now();
            let cli = self.cli()?;
            let work = cli.consume(running.clone(), |event| match event {
                consumer::Event::Ready => self.report(
                    "feishu",
                    "ready",
                    "Listening for bot events".into(),
                    None,
                    true,
                ),
                consumer::Event::Diagnostic(detail) => {
                    self.report("feishu", "error", detail, None, false)
                }
                consumer::Event::Message(result) => {
                    let outcome = result
                        .map_err(|e| Error::Message(e.to_string()))
                        .and_then(|v| self.live_message(v));
                    if let Err(e) = outcome {
                        self.report("feishu", "error", e.to_string(), None, false);
                    }
                }
            });
            futures::pin_mut!(work);
            let monitor = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if self.config()?.is_none() || self.application.state.closing.get() {
                        return Ok::<_, Error>(());
                    }
                }
            };
            futures::pin_mut!(monitor);
            let outcome = tokio::select! {biased;_=stop.cancelled()=>{running.cancel();work.await.map_err(failure)},result=&mut monitor=>{running.cancel();let joined=work.await;result.and(joined.map_err(failure))},result=&mut work=>result.map_err(failure)};
            if let Err(e) = outcome {
                if !running.is_cancelled() {
                    self.report("feishu", "error", e.to_string(), None, false);
                }
            }
            if running.is_cancelled() {
                continue;
            }
            let wait = consumer::retry(
                retry,
                began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            );
            retry = Some(wait);
            pause(wait, &stop).await;
        }
        Ok(())
    }
    pub async fn refresh_names(&self, stop: CancellationToken) -> Result<Value> {
        #[derive(serde::Deserialize)]
        struct Chat {
            chat_id: String,
            #[serde(default)]
            name: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Chats {
            #[serde(default, deserialize_with = "trigger::optional")]
            chats: Option<Vec<Chat>>,
        }
        let value: Chats = serde_json::from_value(
            self.cli()?
                .lark(
                    &[
                        "im".into(),
                        "+chat-list".into(),
                        "--page-all".into(),
                        "--page-size".into(),
                        "100".into(),
                    ],
                    stop,
                )
                .await
                .map_err(failure)?,
        )?;
        let mut labels = JsValue::Object(Vec::new());
        for chat in value.chats.unwrap_or_default() {
            if let Some(name) = chat.name.filter(|n| !n.is_empty()) {
                labels.set(chat.chat_id, JsValue::String(name));
            }
        }
        self.put("source-labels:feishu", &labels)?;
        Ok(serde_json::from_str(&labels.stringify()?)?)
    }
    async fn names_loop(&self, stop: CancellationToken) -> Result<()> {
        if !self.available() {
            return Ok(());
        }
        while !stop.is_cancelled() {
            if self.config()?.is_some() {
                let _outcome = self.refresh_names(stop.clone()).await;
            }
            pause(60_000, &stop).await;
        }
        Ok(())
    }
    fn subscribed(&self) -> Result<Vec<Thread>> {
        Ok(serde_json::from_value(
            self.application
                .state
                .store
                .read("subscribedThreads", &[json!("feishu")])?,
        )?)
    }
    fn latest(&self, thread: &str) -> Result<Option<Trigger>> {
        let items: Vec<Trigger> = serde_json::from_value(
            self.application
                .state
                .store
                .read("threadTriggers", &[json!(thread), json!(1)])?,
        )?;
        Ok(items.into_iter().next())
    }
    fn polled(&self, thread: &Thread) -> Result<bool> {
        let mut at = time::milliseconds(thread.last_at.as_deref().unwrap_or(&thread.created_at))
            .unwrap_or(i64::MIN);
        for id in &thread.subscriptions {
            if let Some(ticket) = self.application.ticket(id)? {
                if live_ticket(&ticket) {
                    return Ok(true);
                }
                if let Some(ended) = ticket.ended.and_then(|e| time::milliseconds(&e.at)) {
                    at = at.max(ended);
                }
            }
        }
        Ok(time::milliseconds(&self.now())
            .unwrap_or(i64::MAX)
            .saturating_sub(at)
            < HORIZON)
    }
    pub async fn backfill_chat(
        &self,
        chat: &str,
        direct: bool,
        since: &str,
        until: &str,
        stop: CancellationToken,
        skipped: &mut usize,
    ) -> Result<()> {
        let topics_key = format!("feishu-history-threads:{chat}");
        let mut topics = self.state(&topics_key)?;
        let mut scan = Scan {
            chat,
            direct,
            since,
            until,
            stop,
            skipped,
        };
        self.scan("chat", chat, &mut topics, &mut scan).await?;
        let mut followed = HashSet::new();
        for thread in self.subscribed()? {
            if self.polled(&thread)? {
                let id = thread
                    .meta
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(self.latest(&thread.key)?.and_then(|t| {
                        t.meta
                            .get("threadId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    }))
                    .unwrap_or_default();
                followed.insert(id);
            }
        }
        let mut pruned = JsValue::Object(Vec::new());
        for (id, at) in topics.ordered_entries() {
            let stale = time::milliseconds(until)
                .zip(at.as_str().and_then(time::milliseconds))
                .is_some_and(|(until, seen)| until.saturating_sub(seen) >= HORIZON);
            if followed.contains(id) || !stale {
                pruned.set(id, at.clone());
            }
        }
        topics = pruned;
        self.put(&topics_key, &topics)?;
        let ids = topics
            .ordered_entries()
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect::<Vec<_>>();
        for id in ids {
            self.scan("thread", &id, &mut topics, &mut scan).await?;
        }
        self.put(&topics_key, &topics)
    }
    async fn scan(
        &self,
        kind: &str,
        id: &str,
        topics: &mut JsValue,
        round: &mut Scan<'_>,
    ) -> Result<()> {
        let (chat, direct, since, until, stop) = (
            round.chat,
            round.direct,
            round.since,
            round.until,
            round.stop.clone(),
        );
        let cli = self.cli()?;
        let bot = cli.deployment().map_err(failure)?;
        let mut page: Option<String> = None;
        let mut pages = HashSet::new();
        loop {
            self.check(&stop)?;
            let seconds = |s: &str| {
                time::milliseconds(s)
                    .map(|v| v.div_euclid(1000).to_string())
                    .ok_or_else(|| Error::Message("Invalid Feishu history cursor time".into()))
            };
            let mut params = JsValue::object([
                ("container_id_type".into(), JsValue::String(kind.into())),
                ("container_id".into(), JsValue::String(id.into())),
                ("start_time".into(), JsValue::String(seconds(since)?)),
                ("end_time".into(), JsValue::String(seconds(until)?)),
                (
                    "sort_type".into(),
                    JsValue::String("ByCreateTimeAsc".into()),
                ),
                ("page_size".into(), JsValue::Number(50.0)),
            ]);
            if let Some(page) = &page {
                params.set("page_token", JsValue::String(page.clone()));
            }
            let result = read::history_page(cli.as_ref(), params.stringify()?, stop.clone())
                .await
                .map_err(failure)?;
            for raw in result.items {
                let message = match serde_json::from_value::<read::Historical>(raw) {
                    Ok(message) => message,
                    Err(_) => {
                        *round.skipped += 1;
                        continue;
                    }
                };
                if message.chat_id != chat
                    || (kind == "thread" && message.thread_id.as_deref() != Some(id))
                {
                    return Err(Error::Message(
                        "Feishu history returned a message outside the requested chat or thread"
                            .into(),
                    ));
                }
                if message.deleted == Some(true) {
                    continue;
                }
                let at = trigger::message_time(&message.create_time).map_err(failure)?;
                if let Some(thread) = &message.thread_id {
                    if !topics
                        .get(thread)
                        .and_then(JsValue::as_str)
                        .and_then(time::milliseconds)
                        .zip(time::milliseconds(&at))
                        .is_some_and(|(old, new)| old >= new)
                    {
                        topics.set(thread, JsValue::String(at));
                    }
                }
                if let Some(trigger) = trigger::normalize(
                    message.event(chat, direct).map_err(failure)?,
                    &bot.bot_id,
                    &bot.app_id,
                )
                .map_err(failure)?
                {
                    self.receive(trigger)?;
                }
            }
            if result.has_more
                && result
                    .page_token
                    .as_ref()
                    .is_none_or(|p| p.is_empty() || pages.contains(p))
            {
                return Err(Error::Message(
                    "Feishu history pagination did not advance".into(),
                ));
            }
            page = result.page_token.filter(|_| result.has_more);
            if let Some(cursor) = &page {
                pages.insert(cursor.clone());
            } else {
                break;
            }
        }
        Ok(())
    }
    fn thread_ref(&self, thread: &Thread) -> Result<Option<read::Reference>> {
        let field = |key: &str| {
            thread
                .meta
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        if let (Some(chat_id), Some(message_id), Some(trigger_id)) = (
            field("chatId"),
            field("messageId"),
            field("sourceTriggerId"),
        ) {
            return Ok(Some(read::Reference {
                trigger_id,
                chat_id,
                message_id,
                at: None,
            }));
        }
        self.latest(&thread.key)?
            .map(|trigger| read::Reference::from_trigger(&trigger).map_err(failure))
            .transpose()
    }
    async fn poll_thread(
        &self,
        thread: &Thread,
        stop: CancellationToken,
        skipped: &mut usize,
    ) -> Result<()> {
        let reference = self.thread_ref(thread)?.ok_or_else(|| {
            Error::Message("Subscribed thread has no retained source message".into())
        })?;
        let Some(config) = self.config()? else {
            return Ok(());
        };
        let cli = self.cli()?;
        let bot = cli.deployment().map_err(failure)?;
        let checkpoint = format!("thread-seen:{}", thread.key);
        let mut known = self
            .application
            .state
            .store
            .get::<Vec<String>>(&checkpoint)?
            .unwrap_or_default();
        let initialized = self
            .application
            .state
            .store
            .get::<bool>(&format!("thread-initialized:{}", thread.key))?
            .unwrap_or(false);
        if !initialized {
            for section in ["root", "parent"] {
                if let read::Read::One {
                    message: Some(message),
                    ..
                } = read::read(
                    cli.as_ref(),
                    &reference,
                    &format!("feishu/{}/{section}", reference.trigger_id),
                    config.read_page_size,
                    "asc",
                    stop.clone(),
                )
                .await
                .map_err(failure)?
                {
                    if !known.contains(&message.message_id) {
                        known.push(message.message_id);
                    }
                }
            }
            self.application.state.store.set(&checkpoint, &known)?;
        }
        let direct = self
            .state("feishu-chats")?
            .get(&reference.chat_id)
            .and_then(JsValue::as_bool)
            == Some(true);
        let since = time::milliseconds(&thread.created_at);
        let root = thread
            .r#ref
            .id
            .split_once(':')
            .map(|(_, root)| root)
            .unwrap_or(&thread.r#ref.id);
        let mut resource = Some(format!("feishu/{}/thread", reference.trigger_id));
        while let Some(value) = resource {
            self.check(&stop)?;
            let read::Read::Page {
                messages,
                next_resource,
                skipped: unreadable,
                thread_id,
                ..
            } = read::read(
                cli.as_ref(),
                &reference,
                &value,
                config.read_page_size,
                "desc",
                stop.clone(),
            )
            .await
            .map_err(failure)?
            else {
                return Err(Error::Message("Expected Feishu thread messages".into()));
            };
            *skipped += unreadable.unwrap_or(0);
            if let Some(id) = thread_id.filter(|s| !s.is_empty()) {
                if thread.meta.get("threadId") != Some(&json!(id)) {
                    self.application
                        .state
                        .store
                        .patch_thread_meta(&thread.key, &[("threadId".into(), Some(json!(id)))])?;
                }
            }
            let encountered = messages.iter().any(|m| known.contains(&m.message_id));
            for message in messages
                .iter()
                .filter(|m| !known.contains(&m.message_id) && m.deleted != Some(true))
                .rev()
            {
                self.check(&stop)?;
                let trigger = trigger::normalize(
                    message
                        .event(&reference.chat_id, direct, root)
                        .map_err(failure)?,
                    &bot.bot_id,
                    &bot.app_id,
                )
                .map_err(failure)?;
                if let Some(trigger) = trigger {
                    if !initialized
                        && time::milliseconds(&trigger.at)
                            .zip(since)
                            .is_some_and(|(at, since)| at < since)
                    {
                        continue;
                    }
                    self.receive(trigger)?;
                }
            }
            for message in messages {
                if !known.contains(&message.message_id) {
                    known.push(message.message_id);
                }
            }
            self.application.state.store.set(&checkpoint, &known)?;
            if initialized && encountered {
                break;
            }
            resource = next_resource;
        }
        self.application
            .state
            .store
            .set(&format!("thread-initialized:{}", thread.key), &true)
    }
    async fn poll_threads(&self, stop: CancellationToken, skipped: &mut usize) -> Result<()> {
        for thread in self.subscribed()? {
            if stop.is_cancelled() {
                return Ok(());
            }
            if !self.polled(&thread)? {
                continue;
            }
            match self.poll_thread(&thread, stop.clone(), skipped).await {
                Ok(()) => self
                    .application
                    .state
                    .store
                    .patch_thread_meta(&thread.key, &[("pollError".into(), None)])?,
                Err(error) => {
                    if stop.is_cancelled() {
                        return Ok(());
                    }
                    self.application.state.store.patch_thread_meta(
                        &thread.key,
                        &[("pollError".into(), Some(json!(error.to_string())))],
                    )?;
                }
            }
        }
        Ok(())
    }
    pub async fn poll_once(&self, stop: CancellationToken) -> Result<()> {
        if !self.available() {
            self.report(
                "history",
                "paused",
                "Waiting for the Feishu bot".into(),
                Some("feishuBotMissing"),
                false,
            );
            return Ok(());
        }
        let Some(config) = self.config()? else {
            self.report("history", "paused", "Off".into(), Some("sourceOff"), false);
            return Ok(());
        };
        let mut chats = JsValue::Object(Vec::new());
        for chat in config.chats {
            chats.set(chat.chat_id, JsValue::Bool(false));
        }
        if config.direct_messages {
            for (id, direct) in self.state("feishu-chats")?.ordered_entries() {
                if direct.as_bool() == Some(true) {
                    chats.set(id, direct.clone());
                }
            }
        }
        let mut errors = Vec::new();
        let mut skipped = 0;
        for (chat, direct) in chats.ordered_entries() {
            if stop.is_cancelled() {
                break;
            }
            let started = self.now();
            let key = format!("feishu-history:{chat}");
            let since = self.application.state.store.get::<String>(&key)?;
            let Some(since) = since else {
                self.application.state.store.set(&key, &started)?;
                continue;
            };
            match self
                .backfill_chat(
                    chat,
                    direct.as_bool() == Some(true),
                    &since,
                    &started,
                    stop.clone(),
                    &mut skipped,
                )
                .await
            {
                Ok(()) => {
                    if !stop.is_cancelled() {
                        let cursor = time::milliseconds(&started)
                            .and_then(|t| time::from_milliseconds(t - 5000).ok())
                            .ok_or_else(|| {
                                Error::Message("Invalid Feishu history start time".into())
                            })?;
                        self.application.state.store.set(&key, &cursor)?;
                    }
                }
                Err(error) => {
                    if !stop.is_cancelled() {
                        errors.push(format!("{chat}: {error}"));
                    }
                }
            }
        }
        self.poll_threads(stop.clone(), &mut skipped).await?;
        if !stop.is_cancelled() {
            if errors.is_empty() {
                self.report(
                    "history",
                    "ready",
                    format!(
                        "Configured groups and known direct chats recovered{}",
                        if skipped > 0 {
                            format!("; {skipped} unreadable messages skipped")
                        } else {
                            String::new()
                        }
                    ),
                    None,
                    true,
                );
            } else {
                self.report("history", "error", errors.join("\n"), None, false);
            }
        }
        Ok(())
    }
    async fn history_loop(&self, stop: CancellationToken) -> Result<()> {
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            self.poll_once(stop.clone()).await?;
            if self.cli.is_none() {
                return Ok(());
            }
            pause(60_000, &stop).await;
        }
    }
    pub async fn once(&self, stop: CancellationToken) -> Result<()> {
        let names = async {
            if self.available() && self.config()?.is_some() {
                let _outcome = self.refresh_names(stop.clone()).await;
            }
            Ok::<_, Error>(())
        };
        let (history, names) = futures::join!(self.poll_once(stop.clone()), names);
        history?;
        names
    }
    pub async fn run(&self, stop: CancellationToken) -> Result<()> {
        let (watch, history, names) = futures::join!(
            self.watch(stop.clone()),
            self.history_loop(stop.clone()),
            self.names_loop(stop)
        );
        watch?;
        history?;
        names
    }
}
async fn pause(ms: u64, stop: &CancellationToken) {
    tokio::select! {_=stop.cancelled()=>(),_=tokio::time::sleep(Duration::from_millis(ms))=>()}
}
