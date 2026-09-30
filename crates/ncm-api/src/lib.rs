//! Client for the NetEase Cloud Music desktop-app protocol ("eapi").
//!
//! Every request is an encrypted `POST /eapi/...` carrying a `header` block that describes the
//! (emulated) desktop client; see [`crypto`] for the wire format.

pub mod api;
pub mod client;
pub mod crypto;
pub mod error;
pub mod link;
pub mod models;
pub mod session;

pub use api::{LoginSecret, QrState};
pub use client::{Client, ClientOptions, install_tls_provider};
pub use error::{Error, Result};
pub use link::{ParsedInput, Resource, ResourceKind, parse_input};
pub use models::{
    AlbumRef, Artist, Availability, Collection, CollectionKind, Level, Lyrics, PlaylistSummary, Privilege, SongUrl, Track,
};
pub use session::{Account, Device, Session};
