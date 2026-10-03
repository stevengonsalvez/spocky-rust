//! Terminal sessions: PTY lifecycle, input and output, resize, snapshots,
//! coalescing, and backpressure over the binary terminal frames, following
//! pinned Paseo `5de45e2`.

pub mod exit_lines;
pub mod input_mode;
pub mod output_coalescer;
pub mod process_title;
pub mod pty;
pub mod restore;
pub mod size_ownership;
pub mod terminal_env;
pub mod utf8_decoder;
pub mod worker_protocol;
