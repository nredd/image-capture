//! Observable portable lifecycle scenarios.
use image_capture::lifecycle::{Action, Lifecycle, Phase, RequestId as Id, TransitionError};

#[test]
fn late_open_closes_only_after_completion() {
    let mut owner = Lifecycle::default();
    assert_eq!(owner.open(Id(1)), Ok(()));
    assert_eq!(owner.request_close(), Action::None);
    assert_eq!(owner.phase(), Phase::Opening(Id(1)));
    assert_eq!(owner.open(Id(2)), Err(TransitionError::Busy));
    assert_eq!(owner.complete(Id(1), true), Action::Close);
    assert_eq!(owner.close(Id(2)), Ok(()));
    assert_eq!(owner.complete(Id(2), true), Action::None);
    assert_eq!(owner.phase(), Phase::Idle);
    assert_eq!(owner.open(Id(3)), Ok(()));
}

#[test]
fn pending_command_and_duplicate_callbacks_cannot_overlap_close() {
    let mut owner = Lifecycle::default();
    assert_eq!(owner.open(Id(1)), Ok(()));
    assert_eq!(owner.complete(Id(1), true), Action::None);
    assert_eq!(owner.command(Id(2)), Ok(()));
    assert_eq!(owner.request_close(), Action::None);
    assert_eq!(owner.close(Id(3)), Err(TransitionError::Busy));
    assert_eq!(owner.complete(Id(1), true), Action::None);
    assert_eq!(owner.phase(), Phase::Command(Id(2)));
    assert_eq!(owner.complete(Id(2), true), Action::Close);
    assert_eq!(owner.close(Id(3)), Ok(()));
    assert_eq!(owner.complete(Id(2), true), Action::None);
    assert_eq!(owner.phase(), Phase::Closing(Id(3)));
    assert_eq!(owner.complete(Id(3), false), Action::None);
    assert_eq!(owner.phase(), Phase::Quarantined);
    assert_eq!(owner.open(Id(4)), Err(TransitionError::Busy));
}

#[test]
fn disconnect_does_not_settle_pending_work() {
    let mut owner = Lifecycle::default();
    assert_eq!(owner.open(Id(1)), Ok(()));
    assert_eq!(owner.complete(Id(1), true), Action::None);
    assert_eq!(owner.command(Id(2)), Ok(()));
    owner.disconnect();
    assert_eq!(owner.phase(), Phase::Command(Id(2)));
    assert_eq!(owner.complete(Id(2), true), Action::None);
    assert_eq!(owner.phase(), Phase::Quarantined);
}

#[test]
fn identity_never_reused_across_sessions() {
    let mut owner = Lifecycle::default();
    assert_eq!(owner.open(Id(1)), Ok(()));
    assert_eq!(owner.complete(Id(1), false), Action::None);
    assert_eq!(owner.open(Id(1)), Err(TransitionError::InvalidIdentity));
    assert_eq!(owner.open(Id(2)), Ok(()));
    assert_eq!(owner.complete(Id(1), true), Action::None);
    assert_eq!(owner.phase(), Phase::Opening(Id(2)));
}
