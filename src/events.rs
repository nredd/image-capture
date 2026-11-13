//! Bounded event delivery, independent of request completion capacity.
#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};

/// An owned event payload.
#[derive(Debug, Eq, PartialEq)]
pub struct Event(pub Vec<u8>);

/// Nonblocking event publication outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventPublish {
    /// Event was queued.
    Delivered,
    /// The queue was full; the receiver must resynchronize.
    Overflow,
    /// The event receiver is gone.
    Disconnected,
    /// The event exceeds the configured limit.
    TooLarge,
}

/// Cloneable producer and shared overflow indicator.
#[derive(Clone, Debug)]
pub struct EventSender {
    sender: SyncSender<Event>,
    overflow: Arc<AtomicBool>,
    max_bytes: usize,
}

/// Event receiver and overflow indicator.
#[derive(Debug)]
pub struct EventReceiver {
    receiver: Receiver<Event>,
    overflow: Arc<AtomicBool>,
}

/// Create a bounded event queue, with a separate sticky overflow indication.
#[must_use]
pub fn event_channel(capacity: usize, max_bytes: usize) -> (EventSender, EventReceiver) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let overflow = Arc::new(AtomicBool::new(false));
    (
        EventSender {
            sender,
            overflow: Arc::clone(&overflow),
            max_bytes,
        },
        EventReceiver { receiver, overflow },
    )
}

impl EventSender {
    /// Try to send an event without blocking; oversized and dropped events are reported.
    #[must_use]
    pub fn publish(&self, event: Event) -> EventPublish {
        if event.0.len() > self.max_bytes {
            self.overflow.store(true, Ordering::Release);
            return EventPublish::TooLarge;
        }
        match self.sender.try_send(event) {
            Ok(()) => EventPublish::Delivered,
            Err(TrySendError::Full(_)) => {
                self.overflow.store(true, Ordering::Release);
                EventPublish::Overflow
            }
            Err(TrySendError::Disconnected(_)) => EventPublish::Disconnected,
        }
    }
}

impl EventReceiver {
    /// Take an event if one is queued.
    ///
    /// # Errors
    /// Returns an error when the queue is empty or all senders disconnected.
    pub fn try_recv(&self) -> Result<Event, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    /// Acknowledge event loss and require the caller to resynchronize state.
    #[must_use]
    pub fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }
}
