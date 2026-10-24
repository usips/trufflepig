//! Connection lifecycle and claim lease configuration.

use super::*;

impl LocalBoard {
    pub fn open(config: &BoardConfig) -> Result<Self, BoardError> {
        Self::open_with_timeout(config, Duration::from_secs(5))
    }

    pub fn open_with_timeout(config: &BoardConfig, timeout: Duration) -> Result<Self, BoardError> {
        config.ensure_local().map_err(BoardError::from)?;
        Self::open_path_with_timeout(
            &config.db_path,
            Duration::from_secs(config.claim_ttl_seconds().cast_unsigned()),
            timeout,
        )
    }

    pub fn open_path(path: &Path, claim_ttl: Duration) -> Result<Self, BoardError> {
        Self::open_path_with_timeout(path, claim_ttl, Duration::from_secs(5))
    }

    pub fn open_path_with_timeout(
        path: &Path,
        claim_ttl: Duration,
        timeout: Duration,
    ) -> Result<Self, BoardError> {
        let (mut conn, path) = board_database::open_with_timeout(path, timeout)?;
        let claim_ttl_secs = i64::try_from(claim_ttl.as_secs())
            .map_err(|_| invalid(BoardErrorCode::InvalidOptions, "claim TTL is too large"))?;
        if claim_ttl_secs == 0 {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                "claim TTL must be positive",
            ));
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let backfilled: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM board_meta WHERE key='v10_claim_backfill')",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !backfilled {
            board_writes::entry_writes::backfill_commit_claim_links(&tx, unix_now()?)?;
            tx.execute(
                "INSERT INTO board_meta(key,value) VALUES('v10_claim_backfill','complete')",
                [],
            )
            .map_err(sql_error)?;
        }
        tx.commit().map_err(sql_error)?;
        let (reader, _) = board_database::open_read_with_timeout(&path, timeout)?;
        Ok(Self {
            conn,
            reader: Some(reader),
            path,
            claim_ttl_secs,
            #[cfg(test)]
            panic_after_write: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_panic_after_write(&mut self) {
        self.panic_after_write = true;
    }

    /// Opens existing storage without creating, migrating, or writing board state.
    pub fn open_read_with_timeout(
        config: &BoardConfig,
        timeout: Duration,
    ) -> Result<Self, BoardError> {
        config.ensure_local().map_err(BoardError::from)?;
        let (conn, path) = board_database::open_read_with_timeout(&config.db_path, timeout)?;
        Ok(Self {
            conn,
            reader: None,
            path,
            claim_ttl_secs: config.claim_ttl_seconds(),
            #[cfg(test)]
            panic_after_write: false,
        })
    }

    pub fn needs_writable_initialization(error: &BoardError) -> bool {
        error.code == BoardErrorCode::BoardInitializationRequired
    }

    pub fn set_claim_ttl(&mut self, claim_ttl: Duration) -> Result<(), BoardError> {
        let seconds = i64::try_from(claim_ttl.as_secs())
            .map_err(|_| invalid(BoardErrorCode::InvalidOptions, "claim TTL is too large"))?;
        if seconds == 0 {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                "claim TTL must be positive",
            ));
        }
        self.claim_ttl_secs = seconds;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stable identity of this database, minted on first writable open.
    /// Read-only handles of stores that never had a writable open report
    /// a missing key instead of minting one.
    pub fn board_uuid(&self) -> Result<String, BoardError> {
        self.conn
            .query_row(
                "SELECT value FROM board_meta WHERE key='board_uuid'",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)
    }

    pub fn set_busy_timeout(&self, timeout: Duration) -> Result<(), BoardError> {
        let timeout = timeout.min(Duration::from_secs(5));
        self.conn.busy_timeout(timeout).map_err(sql_error)?;
        if let Some(reader) = &self.reader {
            reader.busy_timeout(timeout).map_err(sql_error)?;
        }
        Ok(())
    }
}
