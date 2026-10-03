//! `String.prototype.localeCompare` now lives in
//! [`spocky_contracts::js::collate`]; this re-export keeps the callers that
//! still import it from the store working.

pub use spocky_contracts::js::collate::locale_compare;
