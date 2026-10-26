//! Deadline-aware, read-only SQLite dispatch.

#[cfg(test)]
mod tests;

use super::{BoardBackend, LocalBoard, sql_error};
use crate::board_protocol::{BoardError, BoardErrorCode, BoardReply, BoardRequest};
use rusqlite::Connection;
use tp_core::query_deadline::QueryDeadline;

const SQLITE_PROGRESS_OPS: i32 = 1_000;

impl LocalBoard {
    /// Runs a read with SQLite interrupt checks for expiry and caller cancellation.
    pub(crate) fn handle_read_with_deadline(
        &mut self,
        request: &BoardRequest,
        deadline: &QueryDeadline,
    ) -> anyhow::Result<BoardReply> {
        if !request.op.is_read_only() {
            return Err(BoardError::new(
                BoardErrorCode::InvalidOptions,
                "deadline-aware dispatch requires a read-only operation",
            )
            .into());
        }
        with_query_deadline(self, deadline, |board| BoardBackend::handle(board, request))
    }
}

fn with_query_deadline<T>(
    board: &mut LocalBoard,
    deadline: &QueryDeadline,
    operation: impl FnOnce(&mut LocalBoard) -> Result<T, BoardError>,
) -> anyhow::Result<T> {
    let mut guard = QueryDeadlineGuard::install(board, deadline)?;
    let result = operation(&mut *guard.board);
    let expired = deadline.expired();
    let cleared = guard.clear();
    if expired {
        let _ = cleared;
        return Err(query_timeout());
    }
    cleared?;
    Ok(result?)
}

struct QueryDeadlineGuard<'board> {
    board: &'board mut LocalBoard,
    armed: bool,
}

impl<'board> QueryDeadlineGuard<'board> {
    fn install(board: &'board mut LocalBoard, deadline: &QueryDeadline) -> anyhow::Result<Self> {
        if deadline.expired() {
            return Err(query_timeout());
        }
        let progress_deadline = deadline.clone();
        if let Err(error) = progress_connection(board).progress_handler(
            SQLITE_PROGRESS_OPS,
            Some(move || progress_deadline.expired()),
        ) {
            let _ = progress_connection(board).progress_handler(0, None::<fn() -> bool>);
            return Err(anyhow::Error::new(sql_error(error)));
        }
        Ok(Self { board, armed: true })
    }

    fn clear(&mut self) -> anyhow::Result<()> {
        if !self.armed {
            return Ok(());
        }
        progress_connection(self.board)
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(sql_error)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for QueryDeadlineGuard<'_> {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

fn progress_connection(board: &LocalBoard) -> &Connection {
    board.reader.as_ref().unwrap_or(&board.conn)
}

fn query_timeout() -> anyhow::Error {
    anyhow::anyhow!(coded!(TimedOut, "board read deadline expired"))
}
