//! Callback ingress concurrency and bounded-capacity tests.
use icc_ptp_safe::{
    ingress::{Publish, completion_channel},
    lifecycle::RequestId,
};
use std::thread;

#[test]
fn concurrent_duplicate_callbacks_only_publish_once() {
    let (sender, receiver) = completion_channel();
    let callback = sender.request(RequestId(1));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let callback = callback.clone();
            thread::spawn(move || callback.claim().map(|claim| claim.publish(vec![7])))
        })
        .collect();
    let delivered = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .filter(|result| *result == Some(Publish::Delivered))
        .count();
    assert_eq!(delivered, 1);
    assert_eq!(receiver.try_recv().unwrap().payload, vec![7]);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn full_and_disconnected_are_explicit() {
    let (sender, receiver) = completion_channel();
    assert_eq!(
        sender
            .request(RequestId(1))
            .claim()
            .unwrap()
            .publish(vec![]),
        Publish::Delivered
    );
    assert_eq!(
        sender
            .request(RequestId(2))
            .claim()
            .unwrap()
            .publish(vec![]),
        Publish::Full
    );
    drop(receiver);
    assert_eq!(
        sender
            .request(RequestId(3))
            .claim()
            .unwrap()
            .publish(vec![]),
        Publish::Disconnected
    );
}
