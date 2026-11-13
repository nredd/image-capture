//! Owned `ImageCaptureCore` sessions, bounded PTP framing, and callback ownership.
//!
//! macOS native entry is `native::run`; portable modules contain no unsafe code.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(any(target_os = "macos", test))]
pub mod client;
#[cfg(any(target_os = "macos", test))]
mod driver;
pub mod events;
pub mod framing;
pub mod ingress;
pub mod lifecycle;

#[cfg(target_os = "macos")]
pub mod native;
#[cfg(all(target_os = "macos", test))]
mod native_callback_tests;
#[cfg(target_os = "macos")]
mod native_callbacks;
