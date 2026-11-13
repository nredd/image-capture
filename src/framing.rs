//! Bounded PTP command and response container framing.
//!
//! The adapter interprets ICC's first callback buffer as data and second as a
//! PTP response. This is a per-device validation hypothesis, not a header-proven
//! universal guarantee. Response transaction IDs are opaque, not matched to commands.
#![forbid(unsafe_code)]

use std::fmt;

const HEADER_BYTES: usize = 12;
const MAX_PARAMETERS: usize = 5;

/// A malformed or unsupported PTP container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramingError {
    /// The container length or parameter alignment is invalid.
    InvalidLength,
    /// The container is not a PTP response.
    WrongContainerType,
    /// More than five parameters were supplied.
    TooManyParameters,
}

impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => f.write_str("invalid PTP container length"),
            Self::WrongContainerType => f.write_str("not a PTP response container"),
            Self::TooManyParameters => f.write_str("too many PTP parameters"),
        }
    }
}

impl std::error::Error for FramingError {}

/// A parsed response with its opaque framework-provided transaction identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    /// PTP response code.
    pub code: u16,
    /// Opaque transaction identifier.
    pub transaction_id: u32,
    /// Response parameters.
    pub parameters: Vec<u32>,
}

/// Encode an ICC command, with a zero transaction identifier.
///
/// # Errors
/// Returns [`FramingError::TooManyParameters`] for more than five parameters.
pub fn command(operation: u16, parameters: &[u32]) -> Result<Vec<u8>, FramingError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(FramingError::TooManyParameters);
    }
    let length = HEADER_BYTES + parameters.len() * 4;
    // The maximum length is 32, so this conversion is always exact.
    let length = u32::try_from(length).map_err(|_| FramingError::InvalidLength)?;
    let mut bytes = Vec::with_capacity(length as usize);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&operation.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for parameter in parameters {
        bytes.extend_from_slice(&parameter.to_le_bytes());
    }
    Ok(bytes)
}

/// Parse a complete PTP response independently from the data payload.
///
/// # Errors
/// Returns an error if the response is truncated, oversized, misaligned, or
/// has a different container type.
pub fn response(bytes: &[u8]) -> Result<Response, FramingError> {
    if bytes.len() < HEADER_BYTES || bytes.len() > HEADER_BYTES + MAX_PARAMETERS * 4 {
        return Err(FramingError::InvalidLength);
    }
    let declared = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if usize::try_from(declared).ok() != Some(bytes.len())
        || !(bytes.len() - HEADER_BYTES).is_multiple_of(4)
    {
        return Err(FramingError::InvalidLength);
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != 3 {
        return Err(FramingError::WrongContainerType);
    }
    let parameters = bytes[HEADER_BYTES..]
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(u32::from_le_bytes)
        .collect();
    Ok(Response {
        code: u16::from_le_bytes([bytes[6], bytes[7]]),
        transaction_id: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        parameters,
    })
}
