//! Production serialized owner logic, shared by the ICC adapter and controlled tests.
#![forbid(unsafe_code)]
#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
use crate::{
    client::{CommandResult, DeviceId, Error, Operation, Reply, Request},
    framing,
    ingress::{Completion, CompletionSender},
    lifecycle::{Action, Lifecycle, Phase, RequestId},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
    },
    time::Instant,
};

/// Latest admission decision, checked AFTER native block/input preparation.
#[derive(Clone, Copy)]
pub(crate) struct Admission<'a> {
    until: Instant,
    abandoned: &'a AtomicBool,
}
impl<'a> Admission<'a> {
    /// Borrow interest without retaining the request payload in a native block.
    fn new(request: &'a Request) -> Self {
        Self {
            until: request.admit_by,
            abandoned: &request.abandoned,
        }
    }
    /// Check a fresh clock immediately before the prepared native submission.
    pub fn check(self) -> Result<(), Error> {
        if Instant::now() >= self.until || self.abandoned.load(Ordering::Acquire) {
            Err(Error::AdmissionExpired)
        } else {
            Ok(())
        }
    }
}

/// Owner-thread-only native boundary. Submission methods install retained inputs
/// and heap blocks, then check admission immediately before calling ICC. `Err`
/// MUST mean definitely NOT submitted; only then may the driver withdraw state.
/// `Ok` may include a reentrant completion. Never send native objects to a worker.
pub(crate) trait Backend {
    /// Snapshot bounded owned identifiers on the native owner thread.
    fn devices(&self) -> Result<Vec<DeviceId>, Error>;
    /// Retain exactly one selected camera without opening it.
    fn select(&mut self, id: &DeviceId) -> Result<(), Error>;
    /// Prepare retained open state, check fresh admission, then submit once.
    fn open(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        admission: Admission<'_>,
    ) -> Result<(), Error>;
    /// Check current capabilities before preparing a serialized command.
    fn validate_command(&self) -> Result<(), Error>;
    /// Retain copied immutable inputs/block, check fresh admission, submit once.
    fn command(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        command: &[u8],
        outgoing: &[u8],
        admission: Admission<'_>,
    ) -> Result<(), Error>;
    /// Prepare close before checking admission. Cleanup close has no deadline.
    fn close(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        admission: Option<Admission<'_>>,
    ) -> Result<(), Error>;
    /// Drop native request inputs/blocks only after completion or proven non-submission.
    fn settled(&mut self);
    /// Release the camera only in Idle, with no unresolved native request.
    fn release_camera(&mut self);
    /// Observe a sticky removal signal without settling any request.
    fn disconnected(&self) -> bool;
}

/// Serialized production controller; native ownership stays inside its backend.
pub(crate) struct Driver<B> {
    pub backend: B,
    lifecycle: Lifecycle,
    requests: Receiver<Request>,
    completed: Receiver<Completion>,
    sender: CompletionSender,
    pending: Option<Request>,
    discovery: Option<(Instant, Request)>,
    lease: Option<Arc<AtomicBool>>,
    // Keep timeout interest after reply delivery until the next serial request
    // or confirmed close. No native graph or request payload is retained here.
    wait_interest: Option<Arc<AtomicBool>>,
    next: u64,
    stopping: bool,
    fault_reported: bool,
}
impl<B: Backend> Driver<B> {
    /// Construct one controller with independent bounded request/completion queues.
    pub fn new(
        backend: B,
        requests: Receiver<Request>,
        completed: Receiver<Completion>,
        sender: CompletionSender,
    ) -> Self {
        Self {
            backend,
            lifecycle: Lifecycle::default(),
            requests,
            completed,
            sender,
            pending: None,
            discovery: None,
            lease: None,
            wait_interest: None,
            next: 0,
            stopping: false,
            fault_reported: false,
        }
    }
    /// Allocate a process-runtime-wide identity without wrapping or reuse.
    fn identity(&mut self) -> Result<RequestId, Error> {
        self.next = self.next.checked_add(1).ok_or(Error::IdentityExhausted)?;
        Ok(RequestId(self.next))
    }
    /// Stop admission and request closure without cancelling native work.
    pub fn stop(&mut self) {
        self.stopping = true;
    }
    /// Permit owner teardown only after pending work and confirmed closure.
    pub fn finished(&self) -> bool {
        self.stopping && self.lifecycle.phase() == Phase::Idle && self.pending.is_none()
    }
    /// Observe lease drop and timeout interest, including after reply publication.
    fn closing(&self) -> bool {
        self.stopping
            || self.lifecycle.needs_close()
            || self
                .wait_interest
                .as_ref()
                .is_some_and(|interest| interest.load(Ordering::Acquire))
            || self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.load(Ordering::Acquire))
            || self
                .pending
                .as_ref()
                .is_some_and(|request| request.abandoned.load(Ordering::Acquire))
    }
    /// Register a caller or cleanup close before entering potentially reentrant ICC.
    fn begin_close(&mut self, request: Option<Request>) {
        let Ok(id) = self.identity() else {
            self.lifecycle.disconnect();
            if let Some(request) = request {
                let _ = request.reply.try_send(Err(Error::IdentityExhausted));
            }
            return;
        };
        if self.lifecycle.close(id).is_err() {
            return;
        }
        self.pending = request;
        if let Err(error) =
            self.backend
                .close(id, &self.sender, self.pending.as_ref().map(Admission::new))
        {
            self.withdraw(id, error);
        }
    }
    /// Process bounded owner work; `now` is only an early rejection hint.
    pub fn tick(&mut self, now: Instant) {
        let faulted = self.sender.faulted();
        if faulted && !self.fault_reported {
            if let Some(request) = &self.pending {
                let _ = request.reply.try_send(Err(Error::CallbackFailure));
            }
            self.fault_reported = true;
        }
        if self.backend.disconnected() || faulted {
            self.lifecycle.disconnect();
        }
        if self.closing() {
            self.lifecycle.request_close();
        }
        if let Ok(completion) = self.completed.try_recv() {
            self.complete(&completion);
        }
        if self.closing() && self.lifecycle.request_close() == Action::Close {
            self.begin_close(None);
        }
        if self
            .discovery
            .as_ref()
            .is_some_and(|(until, _)| now >= *until || self.stopping)
            && let Some((_, request)) = self.discovery.take()
        {
            let result = if self.stopping {
                Err(Error::Disconnected)
            } else {
                self.backend.devices().map(Reply::Devices)
            };
            let _ = request.reply.try_send(result);
        }
        match self.requests.try_recv() {
            Ok(request) => self.admit(request, now),
            Err(TryRecvError::Disconnected) => self.stopping = true,
            Err(TryRecvError::Empty) => {}
        }
    }
    /// Register one request before backend entry; backend rechecks fresh admission.
    fn admit(&mut self, request: Request, now: Instant) {
        let error = if self.stopping {
            Some(Error::Disconnected)
        } else if request.abandoned.load(Ordering::Acquire) || now >= request.admit_by {
            Some(Error::AdmissionExpired)
        } else if self.lifecycle.phase() == Phase::Quarantined {
            Some(Error::Quarantined)
        } else if self.closing() {
            // Recheck AFTER receiving a new serial worker request. Its enqueue
            // follows any previous Timeout store, so replacement cannot erase
            // that retained interest even if tick's earlier sample missed it.
            Some(Error::Busy)
        } else if self.pending.is_some()
            || self.discovery.is_some()
            || matches!(
                self.lifecycle.phase(),
                Phase::Opening(_) | Phase::Command(_) | Phase::Closing(_)
            )
        {
            Some(Error::Busy)
        } else {
            None
        };
        if let Some(error) = error {
            let _ = request.reply.try_send(Err(error));
            return;
        }
        match &request.operation {
            Operation::Discover(window) => {
                self.discovery = Some((now + *window, request));
            }
            Operation::Open(device, lease) => {
                if self.lifecycle.phase() != Phase::Idle {
                    let _ = request.reply.try_send(Err(Error::Busy));
                    return;
                }
                if let Err(error) = self.backend.select(device) {
                    let _ = request.reply.try_send(Err(error));
                    return;
                }
                let Ok(id) = self.identity() else {
                    self.backend.release_camera();
                    let _ = request.reply.try_send(Err(Error::IdentityExhausted));
                    return;
                };
                if self.lifecycle.open(id).is_err() {
                    return;
                }
                self.lease = Some(Arc::clone(lease));
                self.wait_interest = Some(Arc::clone(&request.abandoned));
                let request = self.pending.insert(request);
                if let Err(error) = self.backend.open(id, &self.sender, Admission::new(request)) {
                    self.withdraw(id, error);
                }
            }
            Operation::Command(_, _) => {
                if self.lifecycle.phase() != Phase::Open {
                    let _ = request.reply.try_send(Err(Error::Busy));
                    return;
                }
                if let Err(error) = self.backend.validate_command() {
                    let _ = request.reply.try_send(Err(error));
                    return;
                }
                let Ok(id) = self.identity() else {
                    let _ = request.reply.try_send(Err(Error::IdentityExhausted));
                    return;
                };
                if self.lifecycle.command(id).is_err() {
                    let _ = request.reply.try_send(Err(Error::Busy));
                    return;
                }
                self.wait_interest = Some(Arc::clone(&request.abandoned));
                // Install the reply/abandonment record BEFORE calling an adapter
                // which may publish synchronously. The owned inputs stay in it.
                let request = self.pending.insert(request);
                let submission = match &request.operation {
                    Operation::Command(command, outgoing) => self.backend.command(
                        id,
                        &self.sender,
                        command,
                        outgoing,
                        Admission::new(request),
                    ),
                    _ => Err(Error::InvalidInput),
                };
                if let Err(error) = submission {
                    self.withdraw(id, error);
                }
            }
            Operation::Close => {
                if self.lifecycle.phase() != Phase::Open {
                    let _ = request.reply.try_send(Err(Error::Busy));
                    return;
                }
                self.begin_close(Some(request));
            }
        }
    }
    /// Withdraw ONLY a backend's definite-not-submitted rejection, never a timeout
    /// of submitted work. The consumed identity is intentionally not reused.
    fn withdraw(&mut self, id: RequestId, error: Error) {
        self.lifecycle.withdraw(id);
        self.backend.settled();
        if let Some(request) = self.pending.take() {
            let _ = request.reply.try_send(Err(error));
        }
        if self.lifecycle.phase() == Phase::Idle {
            self.backend.release_camera();
            self.lease = None;
            self.wait_interest = None;
        }
    }
    /// Settle only a matching request completion, independent of event delivery.
    fn complete(&mut self, completion: &Completion) {
        let phase = self.lifecycle.phase();
        if !matches!(phase, Phase::Opening(id) | Phase::Command(id) | Phase::Closing(id) if id == completion.id)
        {
            return;
        }
        let success = completion.payload.first() == Some(&0);
        let action = self.lifecycle.complete(completion.id, success);
        // Trusted normal API semantics: ICC input last use ends at completion,
        // OR ICC independently owns later use. Headers do not prove that timing.
        // Neither interpretation implies final foreign block disposal.
        self.backend.settled();
        let result = if !success {
            Err(match completion.payload.first() {
                Some(2) => Error::LimitExceeded,
                Some(3) => Error::CallbackFailure,
                _ => Error::NativeFailure,
            })
        } else if self.lifecycle.phase() == Phase::Quarantined {
            Err(Error::Quarantined)
        } else {
            match phase {
                Phase::Opening(_) => Ok(Reply::Opened),
                Phase::Closing(_) => Ok(Reply::Closed),
                Phase::Command(_) => decode_command(&completion.payload).map(Reply::Command),
                _ => Err(Error::InvalidResponse),
            }
        };
        let lost_interest = self.pending.take().is_some_and(|request| {
            request.abandoned.load(Ordering::Acquire) || request.reply.try_send(result).is_err()
        });
        if self.lifecycle.phase() == Phase::Idle {
            self.backend.release_camera();
            self.lease = None;
            self.wait_interest = None;
        }
        if lost_interest {
            self.lifecycle.request_close();
        }
        if action == Action::Close || (lost_interest && self.lifecycle.phase() == Phase::Open) {
            self.begin_close(None);
        }
    }
}

/// Parse the internal owned completion envelope without assuming valid lengths.
pub(crate) fn decode_command(payload: &[u8]) -> Result<CommandResult, Error> {
    let header = payload.get(..5).ok_or(Error::InvalidResponse)?;
    if header[0] != 0 {
        return Err(Error::InvalidResponse);
    }
    let length = usize::try_from(u32::from_le_bytes([
        header[1], header[2], header[3], header[4],
    ]))
    .map_err(|_| Error::InvalidResponse)?;
    if length > 32 || payload.len().saturating_sub(5 + length) > crate::client::MAX_DATA_BYTES {
        return Err(Error::InvalidResponse);
    }
    let end = 5 + length;
    let response = framing::response(payload.get(5..end).ok_or(Error::InvalidResponse)?)
        .map_err(|_| Error::InvalidResponse)?;
    Ok(CommandResult {
        response,
        data: payload.get(end..).ok_or(Error::InvalidResponse)?.to_vec(),
    })
}
