#![forbid(unsafe_code)]

//! Product-domain primitives. Engine and policy parity are tracked separately.
//! Derived from the pinned MIT reference; see THIRD_PARTY_NOTICES.md.

pub mod engine;
pub mod errors;
pub mod ids;
pub mod js_json;
pub mod memory;
pub mod model;
pub mod policy;
pub mod repository;
pub mod settings;
pub mod stage;

pub mod skill_catalog;
pub mod utf16_json;
