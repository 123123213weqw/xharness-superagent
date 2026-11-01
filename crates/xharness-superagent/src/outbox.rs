//! Receipt-backed channel delivery. Network I/O occurs between prepare() and
//! settle(); no SQLite transaction/lock is held over the external Source send.
use crate::{
    effects::{Effects, FAILURES_BEFORE_ERROR},
    store::Store,
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Delivery {
    None,
    Later { reason: String },
    Send { receipt: Value },
}
#[derive(Clone, Copy, Debug)]
pub enum Sent {
    Sent,
    Gone,
    Waiting,
}
pub struct Outbox;
impl Outbox {
    pub fn send(
        store: &Store,
        effects: &mut Effects,
        message: &Value,
        mut validate_im: impl FnMut(&str) -> Result<()>,
    ) -> Result<bool> {
        if message["kind"] == "reply" && message["channel"]["source"] == "feishu" {
            validate_im(message["text"].as_str().unwrap_or(""))?;
        }
        store.atomic(|| {
            let added = store.add_receipt(message)?;
            if added {
                effects.enqueue(
                    store,
                    "reply",
                    message["id"].as_str().unwrap_or(""),
                    &json!({}),
                )?;
            }
            Ok(added)
        })
    }
    pub fn withdraw(store: &Store, effects: &mut Effects, id: &str) -> Result<bool> {
        store.atomic(|| {
            let removed = store.withdraw_receipt(id)?;
            if removed {
                effects.cancel(store, "reply", id)?;
            }
            Ok(removed)
        })
    }
    pub fn prepare(
        store: &Store,
        id: &str,
        mut enabled: impl FnMut(&str) -> Result<bool>,
        mut allowed: impl FnMut(&Value) -> Result<bool>,
        at: &str,
    ) -> Result<Delivery> {
        let Some(receipt) = store.receipt(id)? else {
            return Ok(Delivery::None);
        };
        if receipt["status"] != "pending" {
            return Ok(Delivery::None);
        }
        if !enabled(receipt["channel"]["source"].as_str().unwrap_or(""))? {
            return Ok(Delivery::Later {
                reason: "The Source is off or not installed".into(),
            });
        }
        if store.behind(id, FAILURES_BEFORE_ERROR)? {
            return Ok(Delivery::Later {
                reason: "An earlier message of this ticket to this conversation waits".into(),
            });
        }
        if !allowed(&receipt["channel"])? {
            store.settle_receipt(&receipt, "channelIgnored", at)?;
            return Ok(Delivery::None);
        }
        Ok(Delivery::Send { receipt })
    }
    pub fn settle(store: &Store, message: &Value, outcome: Sent, at: &str) -> Result<Delivery> {
        match outcome {
            Sent::Waiting => Ok(Delivery::Later {
                reason: "The Source is not installed".into(),
            }),
            Sent::Sent | Sent::Gone => {
                store.settle_receipt(
                    message,
                    if matches!(outcome, Sent::Gone) {
                        "channelGone"
                    } else {
                        "sent"
                    },
                    at,
                )?;
                Ok(Delivery::None)
            }
        }
    }
}
