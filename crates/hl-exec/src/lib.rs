//! `hl-exec`: the execution core of the Project Two node.
//!
//! [`Engine::execute_block`] applies one block's order-book actions
//! (`order` GTC/ALO/IOC, `cancel`, `cancelByCloid`, `modify`,
//! `batchModify`) to preallocated per-asset L4 books ([`book::Book`]) and
//! returns per-action responses plus the raw book diffs and fills the block
//! produced, all in reused buffers. Each book keeps its L2 view (level total
//! and order count) incrementally.
//!
//! What is modelled and what is answered `Unsupported` is listed in
//! [`engine`]. A block counts as reproduced only when the replay driver
//! finds our responses and diffs equal to native's; this crate makes no
//! claim about native beyond what its tests show.
//!
//! No floats, no `unsafe` (the workspace forbids it).

pub mod book;
pub mod decimal;
pub mod engine;
pub mod types;

pub use engine::{Engine, EngineConfig, LoadError, ResyncError};
pub use types::*;
