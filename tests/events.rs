//! Event overflow and completion reservation tests.
use icc_ptp_safe::{
    events::{Event, EventPublish, event_channel},
    ingress::{Publish, completion_channel},
    lifecycle::RequestId,
};

#[test]
fn event_overflow_never_consumes_completion_capacity() {
    let (events, received) = event_channel(1, 4);
    let (completion, completed) = completion_channel();
    assert_eq!(events.publish(Event(vec![1])), EventPublish::Delivered);
    assert_eq!(events.publish(Event(vec![2])), EventPublish::Overflow);
    assert!(received.take_overflow());
    assert!(!received.take_overflow());
    assert_eq!(
        completion
            .request(RequestId(1))
            .claim()
            .unwrap()
            .publish(vec![3]),
        Publish::Delivered
    );
    assert_eq!(completed.try_recv().unwrap().payload, vec![3]);
    assert_eq!(received.try_recv().unwrap(), Event(vec![1]));
    assert_eq!(events.publish(Event(vec![0; 5])), EventPublish::TooLarge);
    assert!(received.take_overflow());
}
