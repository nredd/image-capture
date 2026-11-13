//! Portable container framing regression tests.

use icc_ptp_safe::framing::{FramingError, Response, command, response};
use proptest::prelude::*;

#[test]
fn command_framing_and_limit() {
    assert_eq!(
        command(0x1001, &[]),
        Ok(vec![12, 0, 0, 0, 1, 0, 1, 16, 0, 0, 0, 0])
    );
    assert_eq!(
        command(0x1001, &[0; 6]),
        Err(FramingError::TooManyParameters)
    );
}

#[test]
fn response_framing_and_opaque_identifier() {
    let good = [16, 0, 0, 0, 3, 0, 1, 32, 7, 0, 0, 0, 42, 0, 0, 0];
    assert_eq!(
        response(&good),
        Ok(Response {
            code: 0x2001,
            transaction_id: 7,
            parameters: vec![42]
        })
    );
    for end in 0..good.len() {
        assert!(response(&good[..end]).is_err());
    }
    let mut wrong = good;
    wrong[4] = 2;
    assert_eq!(response(&wrong), Err(FramingError::WrongContainerType));
}

proptest! {
    #[test]
    fn arbitrary_inputs_are_total(input in proptest::collection::vec(any::<u8>(), 0..1024)) {
        if let Ok(parsed) = response(&input) {
            prop_assert_eq!(input.len(), 12 + parsed.parameters.len() * 4);
            prop_assert!(parsed.parameters.len() <= 5);
        }
    }

    #[test]
    fn valid_responses_preserve_fields(code in any::<u16>(), id in any::<u32>(), params in proptest::collection::vec(any::<u32>(), 0..=5)) {
        let len = 12_u32 + 4 * u32::try_from(params.len()).unwrap();
        let mut input = len.to_le_bytes().to_vec();
        input.extend(3_u16.to_le_bytes());
        input.extend(code.to_le_bytes());
        input.extend(id.to_le_bytes());
        for parameter in &params { input.extend(parameter.to_le_bytes()); }
        prop_assert_eq!(response(&input), Ok(Response { code, transaction_id: id, parameters: params }));
    }
}
