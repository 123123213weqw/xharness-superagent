//! Trusted configuration ports. Origins and role identity are never model args.
use super::resources::ReadFuture;
use crate::Result;
use std::rc::Rc;
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::{js_json::JsValue, settings::Settings};

pub type ManagementPort = Rc<dyn Fn(String, String, CancellationToken) -> ReadFuture>;
pub type ValidateInstance = Rc<dyn Fn(&str, &JsValue) -> Result<JsValue>>;
pub type ValidatePolicy = Rc<dyn Fn(&JsValue) -> Result<JsValue>>;
pub type SetupFields = Rc<dyn Fn(&Settings) -> Result<JsValue>>;
pub type SyncFuture = futures::future::LocalBoxFuture<'static, Result<JsValue>>;
pub type SyncReady = Rc<dyn Fn(&str, &JsValue) -> bool>;
pub type SyncRun = Rc<dyn Fn(String, JsValue, CancellationToken) -> SyncFuture>;
pub struct PolicySync {
    pub ready: SyncReady,
    pub run: SyncRun,
}
pub struct SourcePorts {
    pub validate_instance: ValidateInstance,
    pub validate_policy: ValidatePolicy,
    pub setup_fields: SetupFields,
    pub sync: Option<PolicySync>,
}
