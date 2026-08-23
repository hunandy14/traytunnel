//! SSH 專屬邏輯的模組群組，目前只有 tunnel 一個成員。
//! `crate::tunnel` 這條舊路徑透過 lib.rs 的 `pub use ssh::tunnel;` 轉口保留，
//! 讓既有呼叫端（commands.rs 等）不必跟著改路徑。
pub mod tunnel;
