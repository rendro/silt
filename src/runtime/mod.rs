//! The runtime objects a value can hold: channels, task and socket
//! handles, and I/O completions; and, in [`sync`], the concurrency core
//! that replaces them.

pub mod channel;
pub mod completion;
pub mod handle;
pub mod sync;
