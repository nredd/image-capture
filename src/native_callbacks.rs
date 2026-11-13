//! Production request-specific native completion blocks.
//!
//! Used by the real ICC adapter and controlled nonhardware tests.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
};

use block2::RcBlock;
use objc2_foundation::{NSData, NSError};

use crate::ingress::CallbackIngress;

pub(crate) type PtpCompletion = dyn Fn(NonNull<NSData>, NonNull<NSData>, *mut NSError);

/// Maximum callback data copied by this experimental adapter.
pub const MAX_DATA_BYTES: usize = crate::client::MAX_DATA_BYTES;
/// Maximum PTP response container copied by this adapter.
pub const MAX_RESPONSE_BYTES: usize = 32;

/// Create a request-specific open or close callback.
///
/// Payload byte `0` denotes success, `1` denotes an error. An error pointer
/// is checked for null but never dereferenced or carried across threads.
#[must_use]
pub fn session_block(ingress: CallbackIngress) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(thread_safe(move |error: *mut NSError| {
        if let Some(claim) = ingress.claim() {
            let _ = claim.publish(vec![u8::from(!error.is_null())]);
        }
    }))
}

/// Statically require invocation AND destruction captures to be thread safe.
fn thread_safe<F: Send + Sync>(closure: F) -> F {
    closure
}

/// Create a request-specific PTP callback.
///
/// The payload begins with a status byte: `0` means success, `1` means the
/// framework returned an error, `2` means a buffer exceeded its limit, and
/// `3` means the adapter caught a Rust panic. Successful payloads then contain
/// a four-byte little-endian response length, response bytes, and data bytes.
///
/// The framework must provide valid borrowed `NSData` objects for the duration
/// of the invocation, as declared by its nonnull callback signature. Callback
/// arguments must not be mutated concurrently during their bounded copy.
///
/// # Safety
/// Every invocation must pass valid `NSData` objects that remain unmodified
/// until the callback returns. The caller must enforce this for the entire
/// lifetime of the returned block, including foreign invocations.
#[must_use]
pub unsafe fn ptp_block(ingress: CallbackIngress) -> RcBlock<PtpCompletion> {
    RcBlock::new(thread_safe(
        move |data: NonNull<NSData>, response: NonNull<NSData>, error: *mut NSError| {
            let Some(claim) = ingress.claim() else {
                return;
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                if !error.is_null() {
                    return vec![1];
                }
                // SAFETY: ImageCaptureCore declares these parameters nonnull NSData
                // objects valid for this callback. Neither reference escapes it.
                let (data, response) = unsafe { (data.as_ref(), response.as_ref()) };
                if data.len() > MAX_DATA_BYTES || response.len() > MAX_RESPONSE_BYTES {
                    return vec![2];
                }
                let response_bytes = response.to_vec();
                let data_bytes = data.to_vec();
                // The response is at most 32 bytes, so this conversion cannot fail.
                let Ok(response_len) = u32::try_from(response_bytes.len()) else {
                    return vec![2];
                };
                let mut payload = Vec::with_capacity(5 + response_bytes.len() + data_bytes.len());
                payload.push(0);
                payload.extend_from_slice(&response_len.to_le_bytes());
                payload.extend_from_slice(&response_bytes);
                payload.extend_from_slice(&data_bytes);
                payload
            }));
            let _ = claim.publish(result.unwrap_or_else(|_| vec![3]));
        },
    ))
}
