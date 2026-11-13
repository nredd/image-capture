//! Bounded, nonblocking completion ingress for request-specific callback blocks.
//!
//! This portable component contains no native values. A callback owns a token,
//! and at most one callback for that token can publish a completion.
#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};

use crate::lifecycle::RequestId;

/// Owned completion data, independent of callback-scoped native buffers.
#[derive(Debug, Eq, PartialEq)]
pub struct Completion {
    /// The request identity captured when the callback was created.
    pub id: RequestId,
    /// Adapter-defined completion payload, already bounded and owned.
    pub payload: Vec<u8>,
}

/// Callback publication result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publish {
    /// The completion was delivered to the reserved slot.
    Delivered,
    /// Another invocation of this request already claimed the slot.
    Duplicate,
    /// The owner has stopped receiving completions.
    Disconnected,
    /// The reserved slot was unexpectedly occupied.
    Full,
}

/// Request-specific callback ingress. Clones share the same once-consumption bit.
#[derive(Clone, Debug)]
pub struct CallbackIngress {
    id: RequestId,
    claimed: Arc<AtomicBool>,
    sender: SyncSender<Completion>,
}

/// Create the dedicated single-slot completion channel.
#[must_use]
pub fn completion_channel() -> (CompletionSender, Receiver<Completion>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    (CompletionSender(sender), receiver)
}

/// Factory for independent request-specific ingress handles.
#[derive(Clone, Debug)]
pub struct CompletionSender(SyncSender<Completion>);

impl CompletionSender {
    /// Create a callback handle for a newly registered request identity.
    #[must_use]
    pub fn request(&self, id: RequestId) -> CallbackIngress {
        CallbackIngress {
            id,
            claimed: Arc::new(AtomicBool::new(false)),
            sender: self.0.clone(),
        }
    }
}

impl CallbackIngress {
    /// Claim this request's completion before copying callback-scoped data.
    ///
    /// The owner must allow only one unresolved request and consume the prior
    /// completion before submitting the next. Otherwise the reserved slot may
    /// be full. A claim is never reissued, even if delivery fails.
    #[must_use]
    pub fn claim(&self) -> Option<CompletionClaim> {
        if self.claimed.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(CompletionClaim {
                id: self.id,
                sender: self.sender.clone(),
            })
        }
    }
}

/// A once-only right to publish a request completion.
#[derive(Debug)]
pub struct CompletionClaim {
    id: RequestId,
    sender: SyncSender<Completion>,
}

impl CompletionClaim {
    /// Publish without blocking. The caller must bound payload size beforehand.
    #[must_use]
    pub fn publish(self, payload: Vec<u8>) -> Publish {
        match self.sender.try_send(Completion {
            id: self.id,
            payload,
        }) {
            Ok(()) => Publish::Delivered,
            Err(TrySendError::Full(_)) => Publish::Full,
            Err(TrySendError::Disconnected(_)) => Publish::Disconnected,
        }
    }
}
