//! Sans-IO ports of the pinned BEAM relay's runtime state machines (`relay@3fc41c9`).
//!
//! Each machine takes explicit inputs (calls, process exits, timer firings, the memory
//! reading) and reports the effects the BEAM process performs (messages to sockets, timers,
//! monitors, metrics). A runner owns the sockets and the clock.
