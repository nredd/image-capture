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

/// Native access stays behind this owner-thread-only interface.
pub(crate) trait Backend {
    fn devices(&self) -> Result<Vec<DeviceId>, Error>;
    fn select(&mut self, id: &DeviceId) -> Result<(), Error>;
    fn open(&mut self, id: RequestId, sender: &CompletionSender);
    fn validate_command(&self) -> Result<(), Error>;
    fn command(
        &mut self,
        id: RequestId,
        sender: &CompletionSender,
        command: &[u8],
        outgoing: &[u8],
    );
    fn close(&mut self, id: RequestId, sender: &CompletionSender);
    fn settled(&mut self);
    fn release_camera(&mut self);
    fn disconnected(&self) -> bool;
}

pub(crate) struct Driver<B> {
    pub backend: B,
    lifecycle: Lifecycle,
    requests: Receiver<Request>,
    completed: Receiver<Completion>,
    sender: CompletionSender,
    pending: Option<Request>,
    discovery: Option<(Instant, Request)>,
    lease: Option<Arc<AtomicBool>>,
    next: u64,
    stopping: bool,
}
impl<B: Backend> Driver<B> {
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
            next: 0,
            stopping: false,
        }
    }
    fn identity(&mut self) -> Result<RequestId, Error> {
        self.next = self.next.checked_add(1).ok_or(Error::IdentityExhausted)?;
        Ok(RequestId(self.next))
    }
    pub fn stop(&mut self) {
        self.stopping = true;
    }
    pub fn finished(&self) -> bool {
        self.stopping && self.lifecycle.phase() == Phase::Idle && self.pending.is_none()
    }
    fn closing(&self) -> bool {
        self.stopping
            || self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.load(Ordering::Acquire))
            || self
                .pending
                .as_ref()
                .is_some_and(|request| request.abandoned.load(Ordering::Acquire))
    }
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
        self.backend.close(id, &self.sender);
    }
    pub fn tick(&mut self, now: Instant) {
        if self.backend.disconnected() {
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
    fn admit(&mut self, request: Request, now: Instant) {
        let error = if self.stopping {
            Some(Error::Disconnected)
        } else if request.abandoned.load(Ordering::Acquire) || now >= request.admit_by {
            Some(Error::AdmissionExpired)
        } else if self.lifecycle.phase() == Phase::Quarantined {
            Some(Error::Quarantined)
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
                self.pending = Some(request);
                self.backend.open(id, &self.sender);
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
                // Install the reply/abandonment record BEFORE calling an adapter
                // which may publish synchronously. The owned inputs stay in it.
                self.pending = Some(request);
                if let Some(Request {
                    operation: Operation::Command(command, outgoing),
                    ..
                }) = &self.pending
                {
                    self.backend.command(id, &self.sender, command, outgoing);
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
    fn complete(&mut self, completion: &Completion) {
        let phase = self.lifecycle.phase();
        if !matches!(phase, Phase::Opening(id) | Phase::Command(id) | Phase::Closing(id) if id == completion.id)
        {
            return;
        }
        let success = completion.payload.first() == Some(&0);
        let action = self.lifecycle.complete(completion.id, success);
        // Completion denotes native last use of request inputs under the trusted
        // ICC API contract. This does not imply final foreign block disposal.
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
