//! Controlled tests exercise the production driver, not a parallel lifecycle mock.
use crate::{
    client::{Client, DeviceId, Error, Operation, Reply, Request, Timeout},
    driver::{Admission, Backend, Driver, decode_command},
    events::event_channel,
    ingress::{CallbackIngress, CompletionSender, completion_channel},
    lifecycle::{Phase, RequestId},
};
use proptest::prelude::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Controlled {
    calls: Vec<&'static str>,
    callback: Option<CallbackIngress>,
    inputs: Vec<u8>,
    removed: bool,
    reentrant: bool,
}
fn id() -> DeviceId {
    DeviceId::new("synthetic-camera".into()).unwrap()
}
impl Controlled {
    fn submit(&mut self, call: &'static str, id: RequestId, sender: &CompletionSender) {
        assert!(self.callback.is_none());
        self.calls.push(call);
        let callback = sender.request(id);
        if self.reentrant {
            let mut payload = vec![0];
            if call == "command" {
                payload.extend([12, 0, 0, 0, 12, 0, 0, 0, 3, 0, 1, 32, 0, 0, 0, 0, 7]);
            }
            let _ = callback.claim().unwrap().publish(payload);
        }
        self.callback = Some(callback);
    }
}
impl Backend for Controlled {
    fn devices(&self) -> Result<Vec<DeviceId>, Error> {
        Ok(vec![id()])
    }
    fn select(&mut self, selected: &DeviceId) -> Result<(), Error> {
        if *selected == id() {
            Ok(())
        } else {
            Err(Error::NotFound)
        }
    }
    fn open(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        admission: Admission<'_>,
    ) -> Result<(), Error> {
        admission.check()?;
        self.submit("open", id, sender);
        Ok(())
    }
    fn validate_command(&self) -> Result<(), Error> {
        Ok(())
    }
    fn command(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        command: &[u8],
        outgoing: &[u8],
        admission: Admission<'_>,
    ) -> Result<(), Error> {
        self.inputs.extend_from_slice(command);
        self.inputs.extend_from_slice(outgoing);
        admission.check()?;
        self.submit("command", id, sender);
        Ok(())
    }
    fn close(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        admission: Option<Admission<'_>>,
    ) -> Result<(), Error> {
        if let Some(admission) = admission {
            admission.check()?;
        }
        self.submit("close", id, sender);
        Ok(())
    }
    fn settled(&mut self) {
        self.callback = None;
        self.inputs.clear();
    }
    fn release_camera(&mut self) {
        self.calls.push("release");
    }
    fn disconnected(&self) -> bool {
        self.removed
    }
}
struct Harness {
    driver: Driver<Controlled>,
    sender: mpsc::SyncSender<Request>,
}
impl Harness {
    fn new() -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (completed, receiver) = completion_channel();
        Self {
            driver: Driver::new(Controlled::default(), requests, receiver, completed),
            sender,
        }
    }
    fn request(
        &mut self,
        operation: Operation,
    ) -> (Receiver<Result<Reply, Error>>, Arc<AtomicBool>) {
        let (reply, received) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.sender
            .try_send(Request {
                operation,
                admit_by: Instant::now() + Duration::from_secs(1),
                abandoned: Arc::clone(&abandoned),
                reply,
            })
            .unwrap();
        self.tick();
        (received, abandoned)
    }
    fn tick(&mut self) {
        self.driver.tick(Instant::now());
    }
    fn complete(&mut self, payload: Vec<u8>) {
        let _ = self
            .driver
            .backend
            .callback
            .as_ref()
            .unwrap()
            .claim()
            .unwrap()
            .publish(payload);
        self.tick();
    }
    fn open(&mut self) -> Arc<AtomicBool> {
        let lease = Arc::new(AtomicBool::new(false));
        let (reply, _) = self.request(Operation::Open(id(), Arc::clone(&lease)));
        self.complete(vec![0]);
        assert!(matches!(reply.try_recv(), Ok(Ok(Reply::Opened))));
        lease
    }
}

#[test]
fn reentrant_open_is_registered_before_submission() {
    let mut h = Harness::new();
    h.driver.backend.reentrant = true;
    let (reply, _) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    assert!(reply.try_recv().is_err());
    h.tick();
    assert!(matches!(reply.try_recv(), Ok(Ok(Reply::Opened))));
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
}
#[test]
fn late_open_after_timeout_closes_then_reopens() {
    let mut h = Harness::new();
    let (reply, abandoned) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    abandoned.store(true, Ordering::Release);
    drop(reply);
    h.tick();
    assert_eq!(h.driver.backend.calls, ["open"]);
    h.complete(vec![0]);
    assert_eq!(h.driver.backend.calls, ["open", "close"]);
    h.complete(vec![0]);
    assert_eq!(h.driver.lifecycle.phase(), Phase::Idle);
    h.open();
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
}
#[test]
fn command_timeout_retains_inputs_until_completion_then_closes() {
    let mut h = Harness::new();
    h.open();
    let (reply, abandoned) = h.request(Operation::Command(vec![1, 2], vec![3, 4]));
    let old = h.driver.backend.callback.clone().unwrap();
    abandoned.store(true, Ordering::Release);
    drop(reply);
    h.tick();
    assert_eq!(h.driver.backend.inputs, [1, 2, 3, 4]);
    assert_eq!(h.driver.backend.calls, ["open", "command"]);
    h.complete(vec![1]);
    assert_eq!(h.driver.backend.inputs, [] as [u8; 0]);
    assert_eq!(h.driver.backend.calls, ["open", "command", "close"]);
    assert!(old.claim().is_none());
    h.tick();
    assert!(matches!(h.driver.lifecycle.phase(), Phase::Closing(_)));
    h.complete(vec![0]);
}
#[test]
fn missing_callback_prevents_shutdown_and_replacement() {
    let mut h = Harness::new();
    let (_reply, _) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    h.driver.stop();
    for _ in 0..10 {
        h.tick();
    }
    assert!(!h.driver.finished());
    assert_eq!(h.driver.backend.calls, ["open"]);
    assert!(h.driver.backend.callback.is_some());
}
#[test]
fn failed_close_and_disconnect_quarantine_without_release() {
    let mut h = Harness::new();
    h.open();
    let (reply, _) = h.request(Operation::Close);
    h.complete(vec![1]);
    assert!(matches!(reply.try_recv(), Ok(Err(Error::NativeFailure))));
    h.driver.stop();
    h.tick();
    assert!(!h.driver.finished());
    assert_eq!(h.driver.lifecycle.phase(), Phase::Quarantined);
    assert!(!h.driver.backend.calls.contains(&"release"));
    let mut h = Harness::new();
    h.open();
    let (_reply, _) = h.request(Operation::Command(vec![1], vec![2]));
    h.driver.backend.removed = true;
    h.tick();
    assert_eq!(h.driver.backend.inputs, [1, 2]);
    h.complete(vec![1]);
    assert_eq!(h.driver.lifecycle.phase(), Phase::Quarantined);
    assert!(!h.driver.backend.calls.contains(&"release"));
}
#[test]
fn admission_expiry_never_calls_backend() {
    let mut h = Harness::new();
    let (reply, received) = mpsc::sync_channel(1);
    h.sender
        .try_send(Request {
            operation: Operation::Open(id(), Arc::new(AtomicBool::new(false))),
            admit_by: Instant::now(),
            abandoned: Arc::new(AtomicBool::new(false)),
            reply,
        })
        .unwrap();
    h.tick();
    assert!(matches!(
        received.try_recv(),
        Ok(Err(Error::AdmissionExpired))
    ));
    assert_eq!(h.driver.backend.calls, [] as [&str; 0]);
}
#[test]
fn lease_drop_closes_and_stale_completion_cannot_settle_close() {
    let mut h = Harness::new();
    let lease = h.open();
    lease.store(true, Ordering::Release);
    h.tick();
    let phase = h.driver.lifecycle.phase();
    h.driver.complete(&crate::ingress::Completion {
        id: RequestId(1),
        payload: vec![0],
    });
    assert_eq!(h.driver.lifecycle.phase(), phase);
    h.complete(vec![0]);
    assert_eq!(h.driver.lifecycle.phase(), Phase::Idle);
}
#[test]
fn checked_identity_exhaustion_never_submits() {
    let mut h = Harness::new();
    h.driver.next = u64::MAX;
    let (reply, _) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    assert!(matches!(
        reply.try_recv(),
        Ok(Err(Error::IdentityExhausted))
    ));
    assert!(!h.driver.backend.calls.contains(&"open"));
}
#[test]
fn worker_panic_requests_close_before_join() {
    let mut h = Harness::new();
    h.open();
    let worker = thread::spawn(|| panic!("controlled worker failure"));
    assert!(worker.join().is_err());
    h.driver.stop();
    h.tick();
    assert_eq!(h.driver.backend.calls, ["open", "close"]);
    assert!(!h.driver.finished());
    h.complete(vec![0]);
    assert!(h.driver.finished());
}
#[test]
fn actual_client_wait_drop_and_bounded_queue() {
    let mut h = Harness::new();
    let (_, events) = event_channel(2, 32);
    let mut client = Client::new(h.sender.clone(), events);
    let worker = thread::spawn(move || {
        let timeout = Timeout {
            admission: Duration::from_millis(10),
            wait: Duration::from_millis(20),
        };
        assert!(matches!(
            client.open(id(), timeout),
            Err(Error::WaitExpired)
        ));
    });
    worker.join().unwrap();
    h.tick(); // Expired, abandoned request must never enter native code.
    assert_eq!(h.driver.backend.calls, [] as [&str; 0]);
}
#[test]
fn completion_envelope_separates_response_from_data() {
    let mut bytes = vec![0, 12, 0, 0, 0];
    bytes.extend([12, 0, 0, 0, 3, 0, 1, 32, 99, 0, 0, 0]);
    bytes.extend([7, 8]);
    let parsed = decode_command(&bytes).unwrap();
    assert_eq!(parsed.response.code, 0x2001);
    assert_eq!(parsed.response.transaction_id, 99);
    assert_eq!(parsed.data, [7, 8]);
    for len in 0..17 {
        assert!(decode_command(&bytes[..len]).is_err());
    }
}
#[test]
fn owned_client_discovers_executes_closes_and_reopens() {
    let mut h = Harness::new();
    h.driver.backend.reentrant = true;
    let (_, events) = event_channel(2, 32);
    let mut client = Client::new(h.sender.clone(), events);
    let worker = thread::spawn(move || {
        let timeout = Timeout {
            admission: Duration::from_secs(1),
            wait: Duration::from_secs(2),
        };
        assert_eq!(
            client.discover(Duration::from_millis(1), timeout).unwrap(),
            [id()]
        );
        for _ in 0..2 {
            let mut session = client.open(id(), timeout).unwrap();
            assert_eq!(
                session.transact(0x1002, &[], vec![], timeout),
                Err(Error::InvalidInput)
            );
            assert_eq!(
                session.transact(0x1003, &[], vec![], timeout),
                Err(Error::InvalidInput)
            );
            let result = session.transact(0x1001, &[], vec![], timeout).unwrap();
            assert_eq!(result.response.code, 0x2001);
            assert_eq!(result.data, [7]);
            session.close(timeout).unwrap();
        }
    });
    let until = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() && Instant::now() < until {
        h.tick();
        thread::sleep(Duration::from_millis(1));
    }
    worker.join().unwrap();
    h.driver.stop();
    h.tick();
    assert!(h.driver.finished());
    assert_eq!(
        h.driver.backend.calls,
        [
            "open", "command", "close", "release", "open", "command", "close", "release"
        ]
    );
}

#[test]
fn timeout_after_reply_publication_retires_lease() {
    let mut h = Harness::new();
    let (reply, abandoned) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    // recv_timeout has decided Timeout, but the receiver is still alive and its
    // worker has not yet published abandonment. Owner delivery wins this gap.
    h.complete(vec![0]);
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
    assert!(h.driver.pending.is_none());
    abandoned.store(true, Ordering::Release);
    drop(reply);
    h.tick();
    assert!(matches!(h.driver.lifecycle.phase(), Phase::Closing(_)));
    h.complete(vec![0]);
    h.open(); // No worker shutdown is needed to recover from the expired open.
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
}

#[test]
fn command_timeout_after_reply_publication_retires_without_shutdown() {
    let mut h = Harness::new();
    h.open();
    let (reply, abandoned) = h.request(Operation::Command(vec![1], vec![2]));
    let mut payload = vec![0, 12, 0, 0, 0];
    payload.extend([12, 0, 0, 0, 3, 0, 1, 32, 0, 0, 0, 0, 7]);
    h.complete(payload);
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
    assert!(h.driver.pending.is_none());
    // Successful native result is queued, then the worker finalizes Timeout.
    abandoned.store(true, Ordering::Release);
    drop(reply);
    h.tick();
    assert!(matches!(h.driver.lifecycle.phase(), Phase::Closing(_)));
    assert_eq!(h.driver.backend.inputs, [] as [u8; 0]);
    h.complete(vec![0]);
    h.open();
    assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
}

#[test]
fn stale_tick_timestamp_cannot_admit_expired_open() {
    let mut h = Harness::new();
    let now = Instant::now();
    let (reply, received) = mpsc::sync_channel(1);
    h.sender
        .try_send(Request {
            operation: Operation::Open(id(), Arc::new(AtomicBool::new(false))),
            admit_by: now.checked_sub(Duration::from_secs(1)).unwrap(),
            abandoned: Arc::new(AtomicBool::new(false)),
            reply,
        })
        .unwrap();
    // Tick sampled before admission expired; native preparation happens later.
    h.driver
        .tick(now.checked_sub(Duration::from_secs(2)).unwrap());
    assert!(matches!(
        received.try_recv(),
        Ok(Err(Error::AdmissionExpired))
    ));
    assert!(!h.driver.backend.calls.contains(&"open"));
    assert_eq!(h.driver.lifecycle.phase(), Phase::Idle);
}

#[test]
fn timeout_interest_survives_immediate_command_retry() {
    let mut h = Harness::new();
    h.open();
    let (reply, abandoned) = h.request(Operation::Command(vec![1], vec![2]));
    h.complete(vec![1]);
    abandoned.store(true, Ordering::Release);
    drop(reply);
    let (reply, received) = mpsc::sync_channel(1);
    // Model a fresh request arriving after tick's first closing sample. The
    // driver's post-receive admission must not replace the old timeout token.
    h.driver.admit(
        Request {
            operation: Operation::Command(vec![1], vec![]),
            admit_by: Instant::now() + Duration::from_secs(1),
            abandoned: Arc::new(AtomicBool::new(false)),
            reply,
        },
        Instant::now(),
    );
    assert!(matches!(received.try_recv(), Ok(Err(Error::Busy))));
    h.tick();
    assert!(matches!(h.driver.lifecycle.phase(), Phase::Closing(_)));
    h.complete(vec![0]);
    h.open();
}

#[test]
fn stale_tick_timestamp_cannot_admit_expired_command_or_close() {
    let mut h = Harness::new();
    h.open();
    for operation in [Operation::Command(vec![1], vec![2]), Operation::Close] {
        let now = Instant::now();
        let (reply, received) = mpsc::sync_channel(1);
        h.sender
            .try_send(Request {
                operation,
                admit_by: now.checked_sub(Duration::from_secs(1)).unwrap(),
                abandoned: Arc::new(AtomicBool::new(false)),
                reply,
            })
            .unwrap();
        h.driver
            .tick(now.checked_sub(Duration::from_secs(2)).unwrap());
        assert!(matches!(
            received.try_recv(),
            Ok(Err(Error::AdmissionExpired))
        ));
        assert_eq!(h.driver.backend.calls, ["open"]);
        assert_eq!(h.driver.backend.inputs, [] as [u8; 0]);
        assert_eq!(h.driver.lifecycle.phase(), Phase::Open);
    }
    // A withdrawn caller close still requests a separate no-deadline cleanup.
    h.tick();
    assert_eq!(h.driver.backend.calls, ["open", "close"]);
}

#[test]
fn completion_overflow_reports_fault_without_releasing_pending() {
    let mut h = Harness::new();
    let (reply, _) = h.request(Operation::Open(id(), Arc::new(AtomicBool::new(false))));
    // Violate the reserved-slot invariant deliberately. Real callback must not
    // silently disappear or free unresolved native ownership after this fault.
    let _ = h
        .driver
        .sender
        .request(RequestId(999))
        .claim()
        .unwrap()
        .publish(vec![0]);
    h.complete(vec![0]);
    assert!(matches!(reply.try_recv(), Ok(Err(Error::CallbackFailure))));
    assert!(h.driver.pending.is_some());
    h.driver.stop();
    h.tick();
    assert!(!h.driver.finished());
    assert_eq!(h.driver.backend.calls, ["open"]);
}

#[test]
fn bounded_queue_and_invalid_client_inputs_do_not_submit() {
    let mut h = Harness::new();
    let (_, events) = event_channel(2, 32);
    let mut client = Client::new(h.sender.clone(), events);
    let (reply, _) = mpsc::sync_channel(1);
    h.sender
        .try_send(Request {
            operation: Operation::Close,
            admit_by: Instant::now() + Duration::from_secs(1),
            abandoned: Arc::new(AtomicBool::new(false)),
            reply,
        })
        .unwrap();
    assert!(matches!(
        client.open(id(), Timeout::default()),
        Err(Error::Busy)
    ));
    assert!(matches!(
        DeviceId::new(String::new()),
        Err(Error::InvalidInput)
    ));
    assert!(matches!(
        DeviceId::new("x".repeat(1025)),
        Err(Error::InvalidInput)
    ));
    assert!(matches!(
        client.open(
            id(),
            Timeout {
                admission: Duration::ZERO,
                wait: Duration::from_secs(1)
            }
        ),
        Err(Error::InvalidInput)
    ));
    assert!(matches!(
        client.discover(Duration::ZERO, Timeout::default()),
        Err(Error::InvalidInput)
    ));
    h.tick();
    assert_eq!(h.driver.backend.calls, [] as [&str; 0]);
}

proptest! {
    #[test]
    fn arbitrary_completion_envelopes_are_total(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = decode_command(&bytes);
    }
}
