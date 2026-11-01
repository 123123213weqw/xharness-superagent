//! Test transport for real Rust Store/Effects operations and JS callback fixtures.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
};
use xharness_superagent::{
    effects::{Effects, Options, Outcome},
    prompts,
    store::Store,
    Error, Result,
};
use xharness_superagent_core::js_json::JsValue;
struct Wire {
    input: BufReader<File>,
    output: File,
}
impl Wire {
    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Err(Error::Message("Fixture pipe closed".into()));
        }
        Ok(serde_json::from_str(&line)?)
    }
    fn send(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.output, value)?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(())
    }
    fn callback(&mut self, method: &str, args: Value, store: &Store) -> Result<Value> {
        self.send(&json!({"kind":"callback","method":method,"args":args}))?;
        loop {
            let result = self.read()?;
            // A real fixture limit/source callback may synchronously read Settings
            // from this same Store. Service that nested read, not a fabricated value,
            // while the outer queue reservation remains owned by its caller.
            if result["kind"] == "command" {
                let nested = nested_read(store, &result);
                let response = match nested {
                    Ok(value) => json!({"kind":"result","value":value,"fired":[]}),
                    Err(error) => {
                        json!({"kind":"result","error":{"message":error.to_string()},"fired":[]})
                    }
                };
                self.send(&response)?;
                continue;
            }
            if let Some(error) = result.get("error") {
                return Err(Error::Message(
                    error["message"]
                        .as_str()
                        .unwrap_or("Fixture callback failed")
                        .into(),
                ));
            }
            return Ok(result["value"].clone());
        }
    }
}
struct Queue {
    queue: Effects,
    clock: Arc<AtomicI64>,
}
struct State {
    store: Option<Store>,
    path: String,
    queues: BTreeMap<u64, Queue>,
    next: u64,
    fired: Arc<Mutex<Vec<Value>>>,
}
fn string(args: &[Value], index: usize) -> &str {
    args.get(index).and_then(Value::as_str).unwrap_or("")
}
fn number(args: &[Value], index: usize, default: i64) -> i64 {
    args.get(index).and_then(Value::as_i64).unwrap_or(default)
}
fn value(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Null)
}
fn undef() -> Value {
    json!({"$undefined":true})
}
fn option(value: Option<Value>) -> Value {
    value.unwrap_or_else(undef)
}
fn nested_read(store: &Store, request: &Value) -> Result<Value> {
    let method = request["method"].as_str().unwrap_or("");
    let args = request["args"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    Ok(match method {
        "settings" => json!({"encoded":store.settings()?.value().stringify()?}),
        "get" => match store.get_raw(string(args, 0))? {
            Some(encoded) => json!({"exists":true,"encoded":encoded}),
            None => json!({"exists":false}),
        },
        "keys" => json!(store.keys(string(args, 0))?),
        "blockRevision" => json!(store.block_revision(string(args, 0))?),
        "aggregate.get" => option(store.aggregate(string(args, 0), string(args, 1))?),
        "disposition.get" => option(store.disposition(string(args, 0))?),
        "receipts.get" => option(store.receipt(string(args, 0))?),
        "receipts.waiting" => json!(store.waiting_receipts()?),
        "read" => store.read(
            string(args, 0),
            value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[]),
        )?,
        "sql.all" => json!(store.query(
            string(args, 0),
            value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[])
        )?),
        "sql.get" => option(
            store
                .query(
                    string(args, 0),
                    value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[]),
                )?
                .into_iter()
                .next(),
        ),
        _ => {
            return Err(Error::Message(format!(
                "Unsupported nested fixture read: {method}"
            )))
        }
    })
}
fn dispatch(state: &mut State, wire: &mut Wire, request: &Value) -> Result<Value> {
    let method = request["method"].as_str().unwrap_or("");
    let args = request["args"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if method == "open" {
        let path = string(args, 0);
        if value(args, 1) == true {
            Store::create_parent(Path::new(path))?;
        }
        state.store = Some(Store::raw(Connection::open(path)?, Box::new(|_| {})));
        state.path = path.into();
        return Ok(undef());
    }
    if method == "close" {
        state.queues.clear();
        if let Some(store) = state.store.take() {
            store.close()?;
        }
        return Ok(undef());
    }
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| Error::Message("Database is closed".into()))?;
    if let Some(method) = method.strip_prefix("prompts.") {
        return prompt_dispatch(store, wire, method, args);
    }
    Ok(match method {
        "initialize" => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if state.path != ":memory:" {
                    std::fs::set_permissions(&state.path, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            let fixed = args.first().and_then(Value::as_i64);
            store.initialize_with_hooks(
                &|| fixed.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
                &mut |_| Ok(()),
            )?;
            undef()
        }
        "begin" => {
            store.begin()?;
            undef()
        }
        "commit" => {
            store.commit()?;
            undef()
        }
        "rollback" => {
            store.rollback()?;
            undef()
        }
        "afterCommit" => {
            let id = value(args, 0);
            let committed = store.in_transaction();
            let fired = state.fired.clone();
            store.after_commit(Box::new(move || {
                fired
                    .lock()
                    .map_err(|e| Error::Message(e.to_string()))?
                    .push(json!({"id":id,"committed":committed}));
                Ok(())
            }))?;
            undef()
        }
        "get" => match store.get_raw(string(args, 0))? {
            Some(encoded) => json!({"exists":true,"encoded":encoded}),
            None => json!({"exists":false}),
        },
        "set" => {
            store.set(string(args, 0), &value(args, 1))?;
            undef()
        }
        "delete" => {
            store.delete(string(args, 0))?;
            undef()
        }
        "keys" => json!(store.keys(string(args, 0))?),
        "sql.exec" => {
            store.connection().execute_batch(string(args, 0))?;
            undef()
        }
        "sql.run" => store.execute(
            string(args, 0),
            value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[]),
        )?,
        "sql.all" => json!(store.query(
            string(args, 0),
            value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[])
        )?),
        "sql.get" => option(
            store
                .query(
                    string(args, 0),
                    value(args, 1).as_array().map(Vec::as_slice).unwrap_or(&[]),
                )?
                .into_iter()
                .next(),
        ),
        "aggregate.get" => option(store.aggregate(string(args, 0), string(args, 1))?),
        "aggregate.save" => {
            store.save(string(args, 0), &value(args, 1))?;
            undef()
        }
        "aggregate.list" => {
            json!(store.list(string(args, 0), args.get(1).and_then(Value::as_str))?)
        }
        "disposition.get" => option(store.disposition(string(args, 0))?),
        "disposition.save" => {
            store.save_disposition(string(args, 0), &value(args, 1))?;
            undef()
        }
        "decisions.byThread" => json!(store.values::<Value>(
            "SELECT value FROM decision WHERE thread=? ORDER BY rowid",
            &[value(args, 0)]
        )?),
        "thread.describe" => {
            store.describe_thread(string(args, 0), &value(args, 1))?;
            undef()
        }
        "thread.patchMeta" => {
            let entries = value(args, 1);
            let entries = entries
                .as_array()
                .ok_or_else(|| Error::Message("Missing patch entries".into()))?;
            let mut patch = Vec::new();
            for entry in entries {
                patch.push((
                    entry[0].as_str().unwrap_or("").into(),
                    if entry[1]["$undefined"] == true {
                        None
                    } else {
                        Some(entry[1].clone())
                    },
                ));
            }
            store.patch_thread_meta(string(args, 0), &patch)?;
            undef()
        }
        "read" => {
            let values = value(args, 1);
            let result = store.read(
                string(args, 0),
                values.as_array().map(Vec::as_slice).unwrap_or(&[]),
            )?;
            if string(args, 0) == "firstTrigger" && result.is_null() {
                undef()
            } else {
                result
            }
        }
        "migration.settings" => {
            let input =
                JsValue::parse(string(args, 0)).map_err(|e| Error::Message(e.to_string()))?;
            json!({"encoded":xharness_superagent::migration::settings::migrate_settings(&input)?.stringify()?})
        }
        "migration.basis" => xharness_superagent::migration::records::basis_of(&value(args, 0)),
        "migration.reasons" => xharness_superagent::migration::records::reasons_of(&value(args, 0)),
        "migration.withoutCopies" => {
            xharness_superagent::migration::records::without_copies(&value(args, 0))?
        }
        "settings" => json!({"encoded":store.settings()?.value().stringify()?}),
        "saveSettings" => {
            let input =
                JsValue::parse(string(args, 0)).map_err(|e| Error::Message(e.to_string()))?;
            let settings = store.save_settings(
                &input,
                args.get(1).filter(|v| !v.is_null()),
                string(args, 2),
            )?;
            json!({"encoded":settings.value().stringify()?})
        }
        "settingsHistory" => {
            let versions = store.settings_history(number(args, 0, 20).max(0) as usize)?;
            let encoded = versions
                .iter()
                .map(JsValue::stringify)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join(",");
            json!({"encoded":format!("[{encoded}]")})
        }
        "blockRevision" => json!(store.block_revision(string(args, 0))?),
        "receipts.get" => option(store.receipt(string(args, 0))?),
        "receipts.waiting" => json!(store.waiting_receipts()?),
        "receipts.add" => json!(store.add_receipt(&value(args, 0))?),
        "receipts.behind" => json!(store.behind(string(args, 0), number(args, 1, 3))?),
        "receipts.settle" => {
            store.settle_receipt(&value(args, 0), string(args, 1), string(args, 2))?;
            undef()
        }
        "receipts.withdraw" => json!(store.withdraw_receipt(string(args, 0))?),
        "outbox.send" => {
            let id = number(args, 0, 0) as u64;
            let queue = state
                .queues
                .get_mut(&id)
                .ok_or_else(|| Error::Message("Unknown queue".into()))?;
            queue
                .clock
                .store(request["now"].as_i64().unwrap_or(0), Ordering::SeqCst);
            xharness_superagent::outbox::Outbox::send(
                store,
                &mut queue.queue,
                &value(args, 1),
                |text| {
                    wire.callback("validateIm", json!([text]), store)?;
                    Ok(())
                },
            )?;
            undef()
        }
        "outbox.withdraw" => {
            let id = number(args, 0, 0) as u64;
            let queue = state
                .queues
                .get_mut(&id)
                .ok_or_else(|| Error::Message("Unknown queue".into()))?;
            json!(xharness_superagent::outbox::Outbox::withdraw(
                store,
                &mut queue.queue,
                string(args, 1)
            )?)
        }
        "outbox.prepare" => {
            let enabled = std::cell::RefCell::new(wire);
            serde_json::to_value(xharness_superagent::outbox::Outbox::prepare(
                store,
                string(args, 0),
                |source| {
                    Ok(enabled
                        .borrow_mut()
                        .callback("sourceEnabled", json!([source]), store)?
                        == true)
                },
                |channel| {
                    Ok(enabled
                        .borrow_mut()
                        .callback("channelAllowed", json!([channel]), store)?
                        == true)
                },
                string(args, 1),
            )?)?
        }
        "outbox.settle" => {
            let outcome = match string(args, 1) {
                "waiting" => xharness_superagent::outbox::Sent::Waiting,
                "gone" => xharness_superagent::outbox::Sent::Gone,
                _ => xharness_superagent::outbox::Sent::Sent,
            };
            serde_json::to_value(xharness_superagent::outbox::Outbox::settle(
                store,
                &value(args, 0),
                outcome,
                string(args, 2),
            )?)?
        }
        "queue.new" => {
            state.next += 1;
            let clock = Arc::new(AtomicI64::new(number(args, 0, 0)));
            let captured = clock.clone();
            let queue = Effects::new(store, Arc::new(move || captured.load(Ordering::SeqCst)))?;
            state.queues.insert(state.next, Queue { queue, clock });
            json!(state.next)
        }
        method if method.starts_with("queue.") => {
            let id = number(args, 0, 0) as u64;
            let queue = state
                .queues
                .get_mut(&id)
                .ok_or_else(|| Error::Message("Unknown queue instance".into()))?;
            queue
                .clock
                .store(request["now"].as_i64().unwrap_or(0), Ordering::SeqCst);
            match method {
                "queue.register" => {
                    let options: Options = serde_json::from_value(value(args, 2))?;
                    queue.queue.register(store, string(args, 1), options)?;
                    undef()
                }
                "queue.enqueue" => {
                    queue.queue.enqueue(
                        store,
                        string(args, 1),
                        string(args, 2),
                        &value(args, 3),
                    )?;
                    undef()
                }
                "queue.cancel" => {
                    queue
                        .queue
                        .cancel(store, string(args, 1), string(args, 2))?;
                    undef()
                }
                "queue.refresh" => {
                    queue.queue.refresh(store, string(args, 1))?;
                    undef()
                }
                "queue.pending" => json!(queue.queue.pending(
                    store,
                    string(args, 1),
                    args.get(2).and_then(Value::as_str)
                )?),
                "queue.start" => {
                    queue.queue.start();
                    undef()
                }
                "queue.drain" => {
                    queue.queue.drain();
                    undef()
                }
                "queue.dispatch" => json!(queue.queue.dispatch(store, |kind| {
                    wire.callback("limit", json!([id, kind]), store)?
                        .as_i64()
                        .ok_or_else(|| Error::Message("Limit must be an integer".into()))
                })?),
                "queue.complete" => {
                    let outcome = match string(args, 2) {
                        "success" => Outcome::Success,
                        "later" => Outcome::Later,
                        _ => Outcome::Failure(string(args, 3).into()),
                    };
                    queue
                        .queue
                        .complete(store, number(args, 1, 0) as u64, outcome)?;
                    undef()
                }
                "queue.failed" => {
                    queue
                        .queue
                        .record_failure(string(args, 1), string(args, 2))?;
                    undef()
                }
                "queue.health" => json!(queue.queue.health()),
                "queue.running" => json!(queue.queue.running()),
                _ => return Err(Error::Message(format!("Unknown queue command: {method}"))),
            }
        }
        _ => return Err(Error::Message(format!("Unknown Store command: {method}"))),
    })
}

struct PromptInputs<'a> {
    store: &'a Store,
    wire: std::cell::RefCell<&'a mut Wire>,
    id: u64,
}
impl PromptInputs<'_> {
    fn call(&self, name: &str) -> Result<Value> {
        self.wire
            .borrow_mut()
            .callback("promptInput", json!([self.id, name]), self.store)
    }
}
impl prompts::Inputs for PromptInputs<'_> {
    fn max_chars(&self) -> Result<usize> {
        self.call("maxChars")?
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| Error::Message("Invalid fixture prompt limit".into()))
    }
    fn environment(&self) -> Result<Option<Vec<prompts::Variable>>> {
        let value = self.call("environment")?;
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_value(value)?))
        }
    }
    fn feishu(&self) -> Result<bool> {
        self.call("feishu")?
            .as_bool()
            .ok_or_else(|| Error::Message("Invalid fixture Feishu flag".into()))
    }
    fn now(&self) -> Result<String> {
        self.call("now")?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Message("Invalid fixture prompt clock".into()))
    }
}
fn prompt_dispatch(store: &Store, wire: &mut Wire, method: &str, args: &[Value]) -> Result<Value> {
    use prompts::{Config, Inputs, PolicyPart, Prompts, Role};
    let parse =
        |index| JsValue::parse(string(args, index)).map_err(|e| Error::Message(e.to_string()));
    let config = |index| Config::parse(&parse(index)?);
    let role = |index| match string(args, index) {
        "worker" => Ok(Role::Worker),
        "orchestrator" => Ok(Role::Orchestrator),
        _ => Err(Error::Message("Invalid fixture prompt role".into())),
    };
    let input = PromptInputs {
        store,
        wire: std::cell::RefCell::new(wire),
        id: args.first().and_then(Value::as_u64).unwrap_or(0),
    };
    let service = Prompts::new(store, &input);
    let result = match method {
        "defaultPrompts" => prompts::default_prompts()?.value(),
        "builtInParts" => JsValue::object(
            prompts::built_in_parts()?
                .iter()
                .map(|p| (p.key.into(), p.value())),
        ),
        "builtInBase" => prompts::built_in_base(string(args, 1))?.into(),
        "promptFile" => prompts::read_prompt_file(Path::new(string(args, 1)))?.into(),
        "normalizeText" => prompts::prompt_file(string(args, 1))?.into(),
        "composePrompt" | "promptParts" => {
            let cfg = config(1)?;
            let role = role(2)?;
            let environment = args.get(3).and_then(Value::as_str);
            let policy = args
                .get(4)
                .filter(|v| v.is_object() && v.get("key").is_some())
                .map(|v| PolicyPart {
                    key: v["key"].as_str().unwrap_or("").into(),
                    text: v["text"].as_str().unwrap_or("").into(),
                });
            if method == "composePrompt" {
                prompts::compose_prompt(&cfg, role, environment, policy.as_ref())?.into()
            } else {
                JsValue::Array(
                    prompts::prompt_parts(&cfg, role, environment, policy.as_ref())?
                        .iter()
                        .map(prompts::Part::value)
                        .collect(),
                )
            }
        }
        "policySection" => {
            let settings = xharness_superagent_core::settings::Settings::parse(&parse(1)?)
                .map_err(|e| Error::Message(e.to_string()))?;
            let cfg = if args.get(2).and_then(Value::as_str).is_some() {
                config(2)?
            } else {
                prompts::default_prompts()?
            };
            prompts::policy_section(&settings, &cfg)?.value()
        }
        "environmentSection" => {
            let vars = args
                .get(2)
                .filter(|v| v.is_array())
                .cloned()
                .map(serde_json::from_value::<Vec<prompts::Variable>>)
                .transpose()?;
            prompts::environment_section(&config(1)?, vars.as_deref())?.into()
        }
        "composeSkill" => prompts::compose_skill(&config(1)?, string(args, 2))?.into(),
        "current" => service.current()?.value(),
        "version" => service.version(number(args, 1, 0) as u64)?.value(),
        "validate" => service.validate(&parse(1)?)?.value(),
        "preview" => service.preview(&parse(1)?)?,
        "defaultChanged" => service.default_changed(&config(1)?)?,
        "save" => service
            .save(&parse(1)?, number(args, 2, 0) as u64, string(args, 3))?
            .value(),
        "history" => service.history(number(args, 1, 0) as u64, number(args, 2, 20) as u64)?,
        "compose" => {
            let vars = args
                .get(3)
                .filter(|v| v.is_array())
                .cloned()
                .map(serde_json::from_value::<Vec<prompts::Variable>>)
                .transpose()?;
            service
                .compose(&config(1)?, role(2)?, vars.as_deref())?
                .into()
        }
        "skills" => JsValue::Array(
            service
                .skills(&config(1)?)?
                .into_iter()
                .map(|(name, body)| JsValue::Array(vec![name.into(), body.into()]))
                .collect(),
        ),
        "inputs" => serde_json::to_string(&input.environment()?)?.into(),
        _ => return Err(Error::Message("Unknown fixture prompt method".into())),
    };
    Ok(json!({"encoded":result.stringify()?}))
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("expected input/output FIFO paths".into());
    }
    let mut wire = Wire {
        input: BufReader::new(OpenOptions::new().read(true).write(true).open(&args[1])?),
        output: OpenOptions::new().read(true).write(true).open(&args[2])?,
    };
    let mut state = State {
        store: None,
        path: String::new(),
        queues: BTreeMap::new(),
        next: 0,
        fired: Arc::new(Mutex::new(Vec::new())),
    };
    loop {
        let request = wire.read()?;
        if request["kind"] == "close" {
            break;
        }
        let result = dispatch(&mut state, &mut wire, &request);
        let mut response = json!({"kind":"result","fired":std::mem::take(&mut *state.fired.lock().map_err(|e|Error::Message(e.to_string()))?)});
        match result {
            Ok(value) => response["value"] = value,
            Err(error) => {
                response["error"] = match &error {
                    Error::PromptUser(user) => {
                        json!({"type":"UserError","user":user,"message":error.to_string()})
                    }
                    Error::PromptValidation(issues) => {
                        json!({"type":"ZodError","issuesEncoded":serde_json::to_string(issues)?,"message":error.to_string()})
                    }
                    Error::PromptConflict { .. } => {
                        json!({"type":"PromptConflict","message":error.to_string()})
                    }
                    _ => json!({"type":"Error","message":error.to_string()}),
                }
            }
        };
        wire.send(&response)?;
    }
    Ok(())
}
