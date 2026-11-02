//! Checkout policy is independent of the optional repository delivery tool.
use crate::{
    sources::github::{CheckoutOptions, CheckoutRevision},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
fn enabled() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Config {
    pub checkout: Checkout,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Checkout {
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    cache: bool,
    #[serde(default)]
    revision: CheckoutRevision,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        self.checkout
            .revision
            .validate()
            .map_err(|e| Error::Message(e.to_string()))
    }
    pub fn plan(&self, state: &Path) -> Plan {
        Plan {
            config: self.checkout.clone(),
            cache_root: state.join("repository-cache"),
        }
    }
}
#[derive(Clone)]
pub(super) struct Plan {
    config: Checkout,
    cache_root: PathBuf,
}
impl Plan {
    pub fn options(&self, trigger: Option<&Value>) -> Result<Option<CheckoutOptions>> {
        if !self.config.enabled {
            return Ok(None);
        }
        let revision = match &self.config.revision {
            CheckoutRevision::Auto => {
                match trigger.filter(|t| t["source"] == "github" && t["meta"]["isPr"] == true) {
                    Some(t) => CheckoutRevision::PullRequest {
                        number: t["meta"]["number"].as_u64().filter(|n| *n > 0).ok_or_else(
                            || Error::Message("GitHub PR trigger lacks a positive number".into()),
                        )?,
                    },
                    None => CheckoutRevision::DefaultBranch,
                }
            }
            value => value.clone(),
        };
        Ok(Some(CheckoutOptions {
            revision,
            cache_root: self.config.cache.then(|| self.cache_root.clone()),
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_trusted_github_pr_metadata_selects_pr_head() {
        let cfg: Config = serde_json::from_value(json!({"checkout":{"cache":true}})).unwrap();
        let plan = cfg.plan(Path::new("/private/state"));
        assert_eq!(
            plan.options(Some(
                &json!({"source":"github","meta":{"isPr":true,"number":12}})
            ))
            .unwrap()
            .unwrap()
            .revision,
            CheckoutRevision::PullRequest { number: 12 }
        );
        assert_eq!(
            plan.options(Some(
                &json!({"source":"feishu","meta":{"isPr":true,"number":12}})
            ))
            .unwrap()
            .unwrap()
            .revision,
            CheckoutRevision::DefaultBranch
        );
        assert!(plan
            .options(Some(&json!({"source":"github","meta":{"isPr":true}})))
            .is_err());
        assert_eq!(
            plan.options(None).unwrap().unwrap().cache_root,
            Some(PathBuf::from("/private/state/repository-cache"))
        );
    }
    #[test]
    fn explicit_disabled_and_revision_configs_are_strict() {
        let cfg: Config = serde_json::from_value(json!({"checkout":{"enabled":false}})).unwrap();
        assert!(cfg
            .plan(Path::new("/state"))
            .options(None)
            .unwrap()
            .is_none());
        for revision in [
            json!({"kind":"branch","name":"../bad"}),
            json!({"kind":"pull_request","number":0}),
            json!({"kind":"commit","sha":"main"}),
        ] {
            let cfg: Config =
                serde_json::from_value(json!({"checkout":{"revision":revision}})).unwrap();
            assert!(cfg.validate().is_err());
        }
        assert!(
            serde_json::from_value::<Config>(json!({"checkout":{"repository":"hardcoded"}}))
                .is_err()
        );
    }
}
