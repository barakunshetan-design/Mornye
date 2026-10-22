//! UniFFI mobile API. This crate is the only backend and routes the app on
//! Android and iOS.
//!
//! Comments that mention "Go" describe the retired Go backend, whose observable
//! behavior (wire formats, parsing quirks, file layouts) remains a compatibility
//! contract.

mod cancellation;
mod data_jobs;
#[cfg(feature = "desktop-host")]
mod desktop;
mod extensions;
mod ffmpeg;
mod filename;
mod hires;
mod library_metadata;
mod logging;
mod lyrics;
mod manager;
mod metadata;
mod native_backup;
mod native_cache;
mod native_collections;
mod native_id3;
mod native_library;
mod native_listing;
mod native_lyrics;
mod native_playlist_write;
mod native_playlists;
mod network_tags;
mod progress;
mod repository;
mod tags;
mod usb_audio;

uniffi::setup_scaffolding!();

#[uniffi::export]
pub fn sanitize_filename(filename: String) -> String {
    mornye_core::filename::sanitize_filename(&filename)
}
