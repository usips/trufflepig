//! Typed board commands and their client-owned text transport.
mod board_operation_parser;
mod board_options;
mod board_text_transport;

pub use board_operation_parser::parse;
pub(crate) use board_operation_parser::validate_board_surface;
pub use board_options::BoardOptions;
pub use board_text_transport::{body_limit, normalize_args, validate_before_body};

use crate::board::{board_ids::BoardRef, board_protocol::BoardOp};

/// Backend operations and commands handled by the client.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardCommand {
    Op(BoardOp),
    Ingest,
    Web { target: Option<BoardRef> },
}
