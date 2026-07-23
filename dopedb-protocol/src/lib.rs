//! Transport-independent contracts shared by the DopeDB Desktop runtime and CLI.
//! This crate deliberately has no database, credential-store, Tauri, or network
//! dependencies so adapters cannot accidentally become a second execution path.

pub mod catalog;
pub mod error;
pub mod frame;
pub mod operation;
pub mod request;
pub mod response;
pub mod version;

pub use catalog::*;
pub use error::{ErrorCode, ProtocolError};
pub use frame::{decode_frame, encode_frame, parse_frame_length, FrameError, FramePayload};
pub use operation::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
pub use request::{CommandName, RequestEnvelope, SessionAuthentication};
pub use response::ResponseEnvelope;
pub use version::{
    negotiate_protocol, ProtocolVersionMismatch, COMMAND_SCHEMA_VERSION, PROTOCOL_MAX, PROTOCOL_MIN,
};

/// Broker request payload cap. Large row/file/terminal streams use dedicated channels.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Broker response payload cap. Query result services apply a smaller semantic cap too.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum accepted JSON nesting before command decoding.
pub const MAX_JSON_DEPTH: usize = 32;
/// Maximum collection length accepted by one control message.
pub const MAX_COLLECTION_ITEMS: usize = 10_000;
/// Maximum total JSON values, including the envelope root.
pub const MAX_JSON_VALUES: usize = 10_000;
/// Maximum UTF-8 bytes accepted in one control-message string.
pub const MAX_STRING_BYTES: usize = 256 * 1024;
