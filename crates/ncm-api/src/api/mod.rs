//! Typed endpoints, implemented as inherent methods on [`crate::Client`].

mod audio;
mod catalog;
mod login;

pub use login::{LoginSecret, QrState};
