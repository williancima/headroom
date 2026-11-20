//! CLIProxyAPI hubs (`docs/cliproxyapi.md`): a server that holds provider sign-ins. Headroom mirrors its
//! login list as accounts and sends each usage read through the hub's `api-call`, where the hub puts the
//! login's token in place of `$TOKEN$`. The management key is the hub's admin key: never log
//! it, and send it only to the address it was saved for.

pub mod client;
pub mod gate;
pub mod keys;
pub mod login;
pub mod manage;
pub mod mirror;
pub mod model;
pub mod transport;
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
mod tests;

pub use client::{unwrap, HubClient, HubRefusal, HubReply, UpstreamRequest};
pub use gate::{reopen, Refused};
pub use keys::{HubKey, HubKeys, KeyringHubKeys, MemoryHubKeys};
pub use login::HubLogin;
pub use manage::{connect, record, remove, replace_key, sync, Synced};
pub use mirror::{mirror, reconcile, Reconciliation};
pub use model::{allows_key, base_url, host_of, Hub, HubLink, TOKEN_PLACEHOLDER};
