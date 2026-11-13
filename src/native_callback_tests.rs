//! Controlled native block invocation without hardware.
use crate::{
    ingress::completion_channel,
    lifecycle::RequestId,
    native_callbacks::{ptp_block, session_block},
};
use objc2_foundation::NSData;
use std::ptr::{self, NonNull};

#[test]
fn session_block_consumes_once() {
    let (sender, receiver) = completion_channel();
    let block = session_block(sender.request(RequestId(1)));
    block.call((ptr::null_mut(),));
    block.call((ptr::null_mut(),));
    assert_eq!(receiver.try_recv().unwrap().payload, vec![0]);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn foreign_retained_session_copy_outlives_wrapper_and_discards_duplicates() {
    let (sender, receiver) = completion_channel();
    let wrapper = session_block(sender.request(RequestId(9)));
    let foreign = wrapper.clone();
    drop(wrapper);
    foreign.call((ptr::null_mut(),));
    assert_eq!(receiver.try_recv().unwrap().payload, [0]);
    drop(receiver);
    foreign.call((ptr::null_mut(),));
    drop(foreign);
}

#[test]
fn late_unsettled_block_after_owner_drop_is_safe() {
    let (sender, receiver) = completion_channel();
    let block = session_block(sender.request(RequestId(10)));
    drop(sender);
    drop(receiver);
    block.call((ptr::null_mut(),));
}

#[test]
fn ptp_oversized_response_and_native_error_are_bounded() {
    let (sender, receiver) = completion_channel();
    let data = NSData::with_bytes(&[]);
    let response = NSData::with_bytes(&[0; 33]);
    // SAFETY: Both invocations receive live, immutable NSData arguments.
    let block = unsafe { ptp_block(sender.request(RequestId(11))) };
    block.call((
        NonNull::from(&*data),
        NonNull::from(&*response),
        ptr::null_mut(),
    ));
    assert_eq!(receiver.try_recv().unwrap().payload, [2]);
    // SAFETY: The invocation below uses the same live immutable data objects.
    let block = unsafe { ptp_block(sender.request(RequestId(12))) };
    // The opaque error is checked for null only, never dereferenced.
    block.call((
        NonNull::from(&*data),
        NonNull::from(&*response),
        ptr::dangling_mut(),
    ));
    assert_eq!(receiver.try_recv().unwrap().payload, [1]);
}

#[test]
fn native_entry_rejects_nonmain_without_creating_runtime() {
    assert_eq!(
        std::thread::spawn(|| crate::native::run(|_| ()))
            .join()
            .unwrap(),
        Err(crate::client::Error::NotMainThread)
    );
}

#[test]
fn production_blocks_preserve_sticky_completion_overflow() {
    let (sender, receiver) = completion_channel();
    let first = session_block(sender.request(RequestId(20)));
    let second = session_block(sender.request(RequestId(21)));
    first.call((ptr::null_mut(),));
    second.call((ptr::null_mut(),));
    assert!(sender.faulted());
    assert_eq!(receiver.try_recv().unwrap().id, RequestId(20));
    assert!(sender.faulted()); // Draining the occupied slot does not erase fault.

    let (sender, _receiver) = completion_channel();
    let first = session_block(sender.request(RequestId(22)));
    first.call((ptr::null_mut(),));
    let data = NSData::with_bytes(&[]);
    // SAFETY: Both callback arguments are the same live immutable empty NSData.
    let ptp = unsafe { ptp_block(sender.request(RequestId(23))) };
    ptp.call((
        NonNull::from(&*data),
        NonNull::from(&*data),
        ptr::null_mut(),
    ));
    assert!(sender.faulted());
}

#[test]
fn ptp_block_copies_owned_response_and_data() {
    let (sender, receiver) = completion_channel();
    // SAFETY: The sole invocation below uses immutable live NSData objects.
    let block = unsafe { ptp_block(sender.request(RequestId(2))) };
    let data = NSData::with_bytes(&[4, 5]);
    let response = NSData::with_bytes(&[1, 2, 3]);
    block.call((
        NonNull::from(&*data),
        NonNull::from(&*response),
        ptr::null_mut(),
    ));
    drop(data);
    drop(response);
    assert_eq!(
        receiver.try_recv().unwrap().payload,
        vec![0, 3, 0, 0, 0, 1, 2, 3, 4, 5]
    );
}
