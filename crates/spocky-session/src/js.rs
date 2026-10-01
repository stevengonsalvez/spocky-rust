//! JavaScript operators the baseline applies to plain values, owned by
//! [`spocky_contracts::js`]. Re-exported for callers that still import them
//! from `spocky_session::js` (`spocky-daemon-app`'s codex agent).

pub use spocky_contracts::js::{js_string, spread, spread_into, truthy};
