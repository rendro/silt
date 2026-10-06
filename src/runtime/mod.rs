//! The runtime objects a value can hold: channels, task and socket
//! handles. What they wait with is in [`sync`], the concurrency core.

pub mod handle;
pub mod sync;
