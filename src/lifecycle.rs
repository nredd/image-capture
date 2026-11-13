//! Portable lifecycle model, independent of any native callback adapter.
//!
//! This model assumes a callback can be attributed to its submitted request.
//! It does not establish that assumption for any native API.
#![forbid(unsafe_code)]

/// Identity allocated by the owner before submitting native work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestId(pub u64);

/// The one active operation or stable session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// No session is open and no native request is pending.
    Idle,
    /// An open request has not completed.
    Opening(RequestId),
    /// The session is open and no command is pending.
    Open,
    /// A command has not completed.
    Command(RequestId),
    /// A close request has not completed.
    Closing(RequestId),
    /// Closure failed or the connection was lost; reuse is prohibited.
    Quarantined,
}

/// Instruction for the owner after processing a completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// No further native submission is requested.
    None,
    /// Submit a close after the completed operation has settled.
    Close,
}

/// A rejected state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionError {
    /// The requested operation cannot be admitted in the current state.
    Busy,
    /// The identity was used before or the identity space is exhausted.
    InvalidIdentity,
}

/// Serialized single-session lifecycle, with caller interest independent of work.
#[derive(Debug)]
pub struct Lifecycle {
    phase: Phase,
    last_id: u64,
    close_requested: bool,
    disconnected: bool,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            last_id: 0,
            close_requested: false,
            disconnected: false,
        }
    }
}

impl Lifecycle {
    /// Current phase, including unresolved native operations.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Consume monotonic identity before state transitions, never wrapping.
    fn admit(&mut self, id: RequestId) -> Result<(), TransitionError> {
        if id.0 <= self.last_id {
            return Err(TransitionError::InvalidIdentity);
        }
        self.last_id = id.0;
        Ok(())
    }

    /// Register an open before native submission.
    ///
    /// # Errors
    /// Returns an error if the owner is not idle or the identity is reused.
    pub fn open(&mut self, id: RequestId) -> Result<(), TransitionError> {
        if self.phase != Phase::Idle {
            return Err(TransitionError::Busy);
        }
        self.admit(id)?;
        self.close_requested = false;
        self.disconnected = false;
        self.phase = Phase::Opening(id);
        Ok(())
    }

    /// Register a command before native submission.
    ///
    /// # Errors
    /// Returns an error if the session is unavailable or the identity is reused.
    pub fn command(&mut self, id: RequestId) -> Result<(), TransitionError> {
        if self.phase != Phase::Open || self.close_requested {
            return Err(TransitionError::Busy);
        }
        self.admit(id)?;
        self.phase = Phase::Command(id);
        Ok(())
    }

    /// Request shutdown without pretending to cancel an active operation.
    pub fn request_close(&mut self) -> Action {
        self.close_requested = true;
        if self.phase == Phase::Open {
            Action::Close
        } else {
            Action::None
        }
    }

    /// Whether stable open state still requires cleanup, including a caller
    /// close that was withdrawn before native submission.
    #[must_use]
    pub fn needs_close(&self) -> bool {
        self.phase == Phase::Open && self.close_requested
    }

    /// Register a close before native submission.
    ///
    /// # Errors
    /// Returns an error if the session is not open or the identity is reused.
    pub fn close(&mut self, id: RequestId) -> Result<(), TransitionError> {
        if self.phase != Phase::Open {
            return Err(TransitionError::Busy);
        }
        self.admit(id)?;
        self.close_requested = true;
        self.phase = Phase::Closing(id);
        Ok(())
    }

    /// Record a disconnect without settling an unrelated pending callback.
    pub fn disconnect(&mut self) {
        self.disconnected = true;
        self.close_requested = true;
        if matches!(self.phase, Phase::Idle | Phase::Open) {
            self.phase = Phase::Quarantined;
        }
    }

    /// Withdraw a registered operation ONLY when the backend proves it was never
    /// submitted. Caller wait expiry is not such proof and must never call this.
    /// Identity remains consumed; cleanup close intent survives withdrawal.
    pub fn withdraw(&mut self, id: RequestId) {
        self.phase = match self.phase {
            Phase::Opening(active) if active == id => Phase::Idle,
            Phase::Command(active) | Phase::Closing(active) if active == id => Phase::Open,
            phase => phase,
        };
    }

    /// Settle the matching operation; stale and duplicate identities are ignored.
    ///
    /// The caller must retain callback-reachable native state separately. A
    /// completion here is not proof of native callback quiescence.
    pub fn complete(&mut self, id: RequestId, success: bool) -> Action {
        match self.phase {
            Phase::Opening(active) if active == id => {
                if !success {
                    self.phase = if self.disconnected {
                        Phase::Quarantined
                    } else {
                        Phase::Idle
                    };
                    Action::None
                } else if self.disconnected {
                    self.phase = Phase::Quarantined;
                    Action::None
                } else {
                    self.phase = Phase::Open;
                    if self.close_requested {
                        Action::Close
                    } else {
                        Action::None
                    }
                }
            }
            Phase::Command(active) if active == id => {
                if self.disconnected {
                    self.phase = Phase::Quarantined;
                    Action::None
                } else {
                    self.phase = Phase::Open;
                    if self.close_requested {
                        Action::Close
                    } else {
                        Action::None
                    }
                }
            }
            Phase::Closing(active) if active == id => {
                self.phase = if success && !self.disconnected {
                    Phase::Idle
                } else {
                    Phase::Quarantined
                };
                Action::None
            }
            _ => Action::None,
        }
    }
}
