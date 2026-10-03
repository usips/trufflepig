//! Configuration snapshots retry on source changes without changing database identity.
use super::{BoardConfig, BoardConfigSource};
use crate::board::board_protocol::{BoardError, BoardErrorCode};
use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

const CONFIG_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const CONFIG_ERROR_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfigStamp {
    Missing,
    File(SystemTime, u64),
    Unreadable(ErrorKind),
}

#[derive(Default)]
pub(crate) struct BoardConfigCache {
    explicit: Option<BoardConfig>,
    source: Option<BoardConfigSource>,
    cached: Option<Result<BoardConfig, BoardError>>,
    stamp: Option<ConfigStamp>,
    next_check: Option<Instant>,
    retry_at: Option<Instant>,
    pinned_database: Option<PathBuf>,
}

impl BoardConfigCache {
    #[cfg(test)]
    pub(crate) fn with_source(path: PathBuf, defaults: BoardConfig) -> Self {
        Self {
            pinned_database: Some(defaults.db_path.clone()),
            source: Some(BoardConfigSource { path, defaults }),
            ..Self::default()
        }
    }

    pub(crate) fn with_config(config: BoardConfig) -> Self {
        Self {
            pinned_database: Some(config.db_path.clone()),
            explicit: Some(config),
            ..Self::default()
        }
    }

    pub(crate) fn snapshot(&self) -> Option<Result<BoardConfig, BoardError>> {
        self.explicit
            .as_ref()
            .map(|config| {
                config
                    .validate()
                    .and_then(|()| config.ensure_local())
                    .map_err(BoardError::from)?;
                Ok(config.clone())
            })
            .or_else(|| self.cached.clone())
    }

    pub(crate) fn needs_refresh(&self, now: Instant) -> bool {
        self.explicit.is_none() && self.next_check.is_none_or(|next| now >= next)
    }

    pub(crate) fn database_path(&self) -> Option<&std::path::Path> {
        self.pinned_database.as_deref()
    }

    pub(crate) fn get(&mut self, now: Instant) -> Result<BoardConfig, BoardError> {
        if let Some(config) = &self.explicit {
            config
                .validate()
                .and_then(|()| config.ensure_local())
                .map_err(BoardError::from)?;
            return Ok(config.clone());
        }
        if self.cached.is_some() && self.next_check.is_some_and(|next| now < next) {
            return self.cached.as_ref().expect("cached snapshot").clone();
        }
        self.next_check = Some(now + CONFIG_CHECK_INTERVAL);
        if self.source.is_none() {
            if self.retry_at.is_some_and(|retry| now < retry) {
                return self.cached.as_ref().expect("cached source failure").clone();
            }
            match BoardConfigSource::from_environment() {
                Ok(source) => {
                    self.pinned_database = Some(source.defaults.db_path.clone());
                    self.source = Some(source);
                }
                Err(error) => {
                    let error = BoardError::from(error);
                    self.cached = Some(Err(error.clone()));
                    self.retry_at = Some(now + CONFIG_ERROR_RETRY);
                    return Err(error);
                }
            }
        }
        let source = self.source.as_ref().expect("source initialized");
        let stamp = match fs::metadata(&source.path) {
            Ok(metadata) => match metadata.modified() {
                Ok(modified) => ConfigStamp::File(modified, metadata.len()),
                Err(error) => ConfigStamp::Unreadable(error.kind()),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => ConfigStamp::Missing,
            Err(error) => ConfigStamp::Unreadable(error.kind()),
        };
        let changed = self.stamp.as_ref() != Some(&stamp);
        let retry = self.retry_at.is_some_and(|retry| now >= retry);
        if self.cached.is_none() || changed || retry {
            let loaded = source.load().map_err(BoardError::from).and_then(|config| {
                if self.pinned_database.as_ref() != Some(&config.db_path) {
                    return Err(BoardError::new(
                        BoardErrorCode::InvalidOptions,
                        "board database differs from the process database pin",
                    ));
                }
                Ok(config)
            });
            self.retry_at = loaded.is_err().then_some(now + CONFIG_ERROR_RETRY);
            self.cached = Some(loaded);
            self.stamp = Some(stamp);
        }
        self.cached.as_ref().expect("configuration loaded").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_configuration_errors_retry_on_mtime_and_keep_the_database_pin() {
        let directory = crate::board::board_test_support::scratch("board-runtime-");
        let path = directory.path().join("board.toml");
        let database = directory.path().join("board.sqlite3");
        fs::write(&path, "broken = true").unwrap();
        let mut cache = BoardConfigCache {
            source: Some(BoardConfigSource {
                path: path.clone(),
                defaults: BoardConfig::for_database(&database),
            }),
            pinned_database: Some(database.clone()),
            ..Default::default()
        };
        let now = Instant::now();
        assert_eq!(
            cache.get(now).unwrap_err().code,
            BoardErrorCode::InvalidOptions
        );
        fs::write(&path, "claim_ttl_minutes = 17").unwrap();
        assert_eq!(
            cache.get(now + Duration::from_millis(1)).unwrap_err().code,
            BoardErrorCode::InvalidOptions
        );
        let recovered = cache.get(now + Duration::from_secs(3)).unwrap();
        assert_eq!(recovered.claim_ttl_minutes, 17);
        assert_eq!(recovered.db_path, database);
        fs::write(&path, "mode = 'remote'").unwrap();
        assert_eq!(
            cache.get(now + Duration::from_secs(6)).unwrap_err().code,
            BoardErrorCode::BoardRemoteUnsupported
        );
        fs::write(&path, "claim_ttl_minutes = 18").unwrap();
        assert_eq!(
            cache.get(now + Duration::from_secs(9)).unwrap().db_path,
            database
        );
    }
}
