//! Block replay: feed real `replica_cmds` blocks to an execution [`engine`],
//! compare every block with what native `hl-node` produced, and time
//! `execute_block` behind the warm gate ([`timing`]).
//!
//! A block counts as reproduced only when the engine's per-action responses
//! AND its book diffs equal native's exactly. Timing is reported over
//! reproduced blocks only; everything else is coverage.

pub mod engine;
pub mod engines;
pub mod gen;
pub mod hlexec;
pub mod native_times;
pub mod replay;
pub mod timing;
pub mod warmbench;
