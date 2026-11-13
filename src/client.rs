//! Owned, thread-safe request API. Native objects never enter this module.
#![forbid(unsafe_code)]

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use crate::{
    events::EventReceiver,
    framing::{self, Response},
};

/// Maximum incoming or outgoing command data, excluding the response container.
pub const MAX_DATA_BYTES: usize = 16 * 1024 * 1024;
/// Maximum discovery entries returned by the wrapper.
pub const MAX_DEVICES: usize = 64;
/// Maximum UTF-8 bytes in a device identifier.
pub const MAX_ID_BYTES: usize = 1024;

/// Nonidentifying wrapper errors. Framework error text is never propagated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Native entry must occur on the main thread.
    NotMainThread,
    /// A runtime was already constructed in this process.
    AlreadyCreated,
    /// Invalid deadline, identifier, operation, or payload bounds.
    InvalidInput,
    /// The bounded request slot or single session is occupied.
    Busy,
    /// Admission expired before native submission.
    AdmissionExpired,
    /// Caller stopped waiting. Native work is NOT cancelled.
    WaitExpired,
    /// Worker or owner communication ended.
    Disconnected,
    /// Exact selection found no matching camera.
    NotFound,
    /// Exact selection found multiple matching cameras.
    Ambiguous,
    /// A discovery or callback bound was exceeded.
    LimitExceeded,
    /// Native completion reported an error.
    NativeFailure,
    /// A native buffer or PTP container was malformed.
    InvalidResponse,
    /// A callback adapter caught an internal failure.
    CallbackFailure,
    /// The native graph cannot be reused or reclaimed safely.
    Quarantined,
    /// Monotonic request identity space is exhausted.
    IdentityExhausted,
    /// The joined worker panicked.
    WorkerPanicked,
    /// The OS could not create the owned worker.
    WorkerStartFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ICC boundary: {self:?}")
    }
}
impl std::error::Error for Error {}

/// Exact framework identifier. Never interpreted as a substring or display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceId(String);
impl DeviceId {
    /// Validate an exact, nonempty identifier.
    ///
    /// # Errors
    /// Returns `InvalidInput` for empty, oversized, or NUL-containing identifiers.
    pub fn new(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.len() > MAX_ID_BYTES || value.contains('\0') {
            return Err(Error::InvalidInput);
        }
        Ok(Self(value))
    }
    /// Borrow the exact identifier. Avoid logging private device identities.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Relative admission and caller-wait limits, each at most one hour.
#[derive(Clone, Copy, Debug)]
pub struct Timeout {
    /// Latest permitted admission, relative to enqueueing.
    pub admission: Duration,
    /// Latest caller wait, relative to enqueueing; not cancellation.
    pub wait: Duration,
}
impl Default for Timeout {
    fn default() -> Self {
        Self {
            admission: Duration::from_secs(5),
            wait: Duration::from_secs(30),
        }
    }
}
impl Timeout {
    fn deadlines(self) -> Result<(Instant, Instant), Error> {
        if self.admission.is_zero()
            || self.wait.is_zero()
            || self.admission > self.wait
            || self.wait > Duration::from_secs(3600)
        {
            return Err(Error::InvalidInput);
        }
        let now = Instant::now();
        Ok((
            now.checked_add(self.admission).ok_or(Error::InvalidInput)?,
            now.checked_add(self.wait).ok_or(Error::InvalidInput)?,
        ))
    }
}

/// Owned response and separate data buffer. ICC transaction IDs are opaque.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResult {
    /// Parsed PTP response, including non-success device response codes.
    pub response: Response,
    /// Separate incoming data, without an assumed USB data header.
    pub data: Vec<u8>,
}

pub(crate) enum Operation {
    Discover(Duration),
    Open(DeviceId, Arc<AtomicBool>),
    Command(Vec<u8>, Vec<u8>),
    Close,
}
pub(crate) enum Reply {
    Devices(Vec<DeviceId>),
    Opened,
    Command(CommandResult),
    Closed,
}
pub(crate) struct Request {
    pub operation: Operation,
    pub admit_by: Instant,
    pub abandoned: Arc<AtomicBool>,
    pub reply: SyncSender<Result<Reply, Error>>,
}

/// Single owned worker endpoint. It has no native objects and is `Send`, not cloneable.
pub struct Client {
    sender: SyncSender<Request>,
    events: EventReceiver,
}
impl Client {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn new(sender: SyncSender<Request>, events: EventReceiver) -> Self {
        Self { sender, events }
    }

    fn request(&mut self, operation: Operation, timeout: Timeout) -> Result<Reply, Error> {
        let (admit_by, wait_until) = timeout.deadlines()?;
        let (reply, received) = mpsc::sync_channel(1);
        let abandoned = Arc::new(AtomicBool::new(false));
        self.sender
            .try_send(Request {
                operation,
                admit_by,
                abandoned: Arc::clone(&abandoned),
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::Disconnected,
            })?;
        match received.recv_timeout(wait_until.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                abandoned.store(true, Ordering::Release);
                Err(Error::WaitExpired)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        }
    }

    /// Observe a bounded discovery window, then return exact camera identifiers.
    ///
    /// # Errors
    /// Returns deadline, queue, discovery-limit, or connection errors. An empty
    /// result may mean no cameras, denied permission, or unfinished discovery.
    pub fn discover(&mut self, window: Duration, timeout: Timeout) -> Result<Vec<DeviceId>, Error> {
        if window.is_zero() || window > Duration::from_secs(60) || window >= timeout.wait {
            return Err(Error::InvalidInput);
        }
        match self.request(Operation::Discover(window), timeout)? {
            Reply::Devices(devices) => Ok(devices),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Acquire the only session lease by exact identifier.
    ///
    /// # Errors
    /// Returns selection, deadline, native or lifecycle errors. An expired wait
    /// can leave an opening session; it will close only after its completion.
    pub fn open(&mut self, id: DeviceId, timeout: Timeout) -> Result<Session<'_>, Error> {
        let close = Arc::new(AtomicBool::new(false));
        match self.request(Operation::Open(id, Arc::clone(&close)), timeout)? {
            Reply::Opened => Ok(Session {
                client: self,
                close,
            }),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Access bounded, uncorrelated advisory events and their overflow flag.
    #[must_use]
    pub const fn events(&self) -> &EventReceiver {
        &self.events
    }
}

/// Exclusive owned session lease. Drop requests close without cancelling work.
pub struct Session<'a> {
    client: &'a mut Client,
    close: Arc<AtomicBool>,
}
impl Session<'_> {
    /// Submit a serialized PTP command. Raw PTP Open/CloseSession is prohibited.
    ///
    /// # Errors
    /// Returns bounds, framing, deadline, lifecycle, or native errors. Timeout
    /// retires the lease after pending work settles; it never cancels the request.
    pub fn transact(
        &mut self,
        operation: u16,
        parameters: &[u32],
        outgoing: Vec<u8>,
        timeout: Timeout,
    ) -> Result<CommandResult, Error> {
        if matches!(operation, 0x1002 | 0x1003) || outgoing.len() > MAX_DATA_BYTES {
            return Err(Error::InvalidInput);
        }
        let command = framing::command(operation, parameters).map_err(|_| Error::InvalidInput)?;
        match self
            .client
            .request(Operation::Command(command, outgoing), timeout)?
        {
            Reply::Command(result) => Ok(result),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Confirm close before releasing the lease. Failure may quarantine forever.
    ///
    /// # Errors
    /// Returns deadline, connection, or native close errors. Drop still requests
    /// close; it does not release unresolved native ownership.
    pub fn close(self, timeout: Timeout) -> Result<(), Error> {
        match self.client.request(Operation::Close, timeout)? {
            Reply::Closed => Ok(()),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Access advisory events; these never complete a transaction.
    #[must_use]
    pub const fn events(&self) -> &EventReceiver {
        self.client.events()
    }
}
impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.close.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn channel(events: EventReceiver) -> (Client, mpsc::Receiver<Request>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    (Client::new(sender, events), receiver)
}
