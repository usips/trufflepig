//! Typed board commands and their client-owned text transport.
mod board_operation_parser;
mod board_options;
mod board_text_transport;

pub use board_operation_parser::parse;
pub use board_options::BoardOptions;
pub use board_text_transport::{body_limit, normalize_args, validate_before_body};

use crate::board::board_protocol::BoardOp;

/// Backend operations and the one local git-ingestion command.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardCommand {
    Op(BoardOp),
    Ingest,
}
