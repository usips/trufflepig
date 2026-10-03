//! Strict board configuration with passwd-derived identity and durable data paths.
use super::board_actor::{BoardActor, HarnessLabel, validate_actor_component};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub const DEFAULT_CLAIM_TTL_MINUTES: u64 = 120;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardMode {
    #[default]
    Local,
    Remote,
}

#[derive(Clone, Debug)]
pub struct BoardConfig {
    pub mode: BoardMode,
    pub user: String,
    pub host: String,
    pub db_path: PathBuf,
    pub url: Option<String>,
    pub token_file: Option<PathBuf>,
    pub claim_ttl_minutes: u64,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardConfigFile {
    mode: Option<BoardMode>,
    user: Option<String>,
    host: Option<String>,
    url: Option<String>,
    token_file: Option<PathBuf>,
    claim_ttl_minutes: Option<u64>,
}

impl BoardConfig {
    pub fn load() -> Result<Self> {
        let account = passwd_account()?;
        let config_home = xdg_home("XDG_CONFIG_HOME", account.home.join(".config"));
        let data_home = xdg_home("XDG_DATA_HOME", account.home.join(".local/share"));
        let mut config = Self {
            mode: BoardMode::Local,
            user: account.user,
            host: machine_hostname()?,
            db_path: data_home.join("trufflepig/board.sqlite3"),
            url: None,
            token_file: None,
            claim_ttl_minutes: DEFAULT_CLAIM_TTL_MINUTES,
        };
        let config_path = config_home.join("trufflepig/board.toml");
        match fs::read_to_string(&config_path) {
            Ok(input) => config = Self::from_toml(&input, config)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("invalid_options: read {}", config_path.display()));
            }
        }
        if let Some(path) = env::var_os("TRUFFLEPIG_BOARD_DB") {
            if path.is_empty() {
                bail!("invalid_options: TRUFFLEPIG_BOARD_DB must not be empty");
            }
            config.db_path = PathBuf::from(path);
        }
        if !config.db_path.is_absolute() {
            config.db_path = env::current_dir()?.join(&config.db_path);
        }
        config.validate()?;
        config.ensure_local()?;
        Ok(config)
    }

    /// Resolve a strict TOML document against supplied defaults without reading env.
    pub fn from_toml(input: &str, mut defaults: Self) -> Result<Self> {
        let file: BoardConfigFile =
            toml::from_str(input).context("invalid_options: invalid board.toml")?;
        if let Some(mode) = file.mode {
            defaults.mode = mode;
        }
        if let Some(user) = file.user {
            defaults.user = user;
        }
        if let Some(host) = file.host {
            defaults.host = host;
        }
        if let Some(ttl) = file.claim_ttl_minutes {
            defaults.claim_ttl_minutes = ttl;
        }
        defaults.url = file.url;
        defaults.token_file = file.token_file;
        defaults.validate()?;
        Ok(defaults)
    }

    /// Test/local embedding configuration; does not inspect or mutate process env.
    pub fn for_database(path: impl AsRef<Path>) -> Self {
        Self {
            mode: BoardMode::Local,
            user: "test".into(),
            host: "localhost".into(),
            db_path: path.as_ref().to_path_buf(),
            url: None,
            token_file: None,
            claim_ttl_minutes: DEFAULT_CLAIM_TTL_MINUTES,
        }
    }

    pub fn actor(&self, client: Option<&str>, session: Option<&str>) -> Result<BoardActor> {
        BoardActor::new(
            &self.user,
            &self.host,
            HarnessLabel::parse(client.unwrap_or("cli"))?,
            session.unwrap_or("unattributed"),
        )
    }

    pub fn claim_ttl_seconds(&self) -> i64 {
        (self.claim_ttl_minutes * 60) as i64
    }

    pub fn ensure_local(&self) -> Result<()> {
        if self.mode == BoardMode::Remote {
            bail!(
                "board_remote_unsupported: remote coordinator is not available; use mode = \"local\""
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        validate_actor_component(&self.user, "user")?;
        validate_actor_component(&self.host, "host")?;
        if self.claim_ttl_minutes == 0 || self.claim_ttl_minutes > (i64::MAX as u64) / 60 {
            bail!("invalid_options: claim_ttl_minutes must be positive and fit Unix seconds");
        }
        if self.db_path.as_os_str().is_empty() {
            bail!("invalid_options: board database path must not be empty");
        }
        if let Some(url) = &self.url {
            if url.is_empty() || url.len() > 2048 || url.chars().any(char::is_control) {
                bail!("invalid_options: invalid board URL");
            }
        }
        Ok(())
    }
}

fn xdg_home(name: &str, default: PathBuf) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or(default)
}

struct PasswdAccount {
    user: String,
    home: PathBuf,
}

#[cfg(unix)]
fn passwd_account() -> Result<PasswdAccount> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    let mut buffer = vec![0u8; 16384];
    loop {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // getpwuid_r writes the record and pointers into the owned buffer; both outlive all reads below.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1048576 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 {
            bail!(
                "invalid_actor: passwd lookup failed: {}",
                std::io::Error::from_raw_os_error(status)
            );
        }
        if result.is_null() {
            bail!("invalid_actor: current uid has no passwd entry");
        }
        // A non-null successful result guarantees initialization of the passwd record.
        let record = unsafe { record.assume_init() };
        if record.pw_name.is_null() || record.pw_dir.is_null() {
            bail!("invalid_actor: passwd entry lacks user or home");
        }
        // These NUL-terminated strings belong to buffer and are copied before it is dropped.
        let user = unsafe { CStr::from_ptr(record.pw_name) }
            .to_str()
            .context("invalid_actor: passwd user is not UTF-8")?
            .to_owned();
        // pw_dir is a filesystem path and may contain non-UTF-8 bytes.
        let home = unsafe { CStr::from_ptr(record.pw_dir) };
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes()));
        if !home.is_absolute() {
            bail!("invalid_actor: passwd home is not absolute");
        }
        validate_actor_component(&user, "user")?;
        return Ok(PasswdAccount { user, home });
    }
}

#[cfg(unix)]
fn machine_hostname() -> Result<String> {
    let mut buffer = [0u8; 256];
    // gethostname receives a writable fixed-size buffer with its exact capacity.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error())
            .context("invalid_actor: hostname lookup failed");
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .context("invalid_actor: hostname exceeds 255 bytes")?;
    let host = std::str::from_utf8(&buffer[..length])
        .context("invalid_actor: hostname is not UTF-8")?
        .to_owned();
    validate_actor_component(&host, "host")?;
    Ok(host)
}

#[cfg(not(unix))]
fn passwd_account() -> Result<PasswdAccount> {
    bail!("invalid_actor: board passwd identity requires a Unix host")
}
#[cfg(not(unix))]
fn machine_hostname() -> Result<String> {
    bail!("invalid_actor: board hostname capture requires a Unix host")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_config_rejects_unknown_fields_and_invalid_ttl() {
        let defaults = || BoardConfig::for_database("target/board-config-test.sqlite3");
        assert!(BoardConfig::from_toml("usre = 'josh'", defaults()).is_err());
        assert!(BoardConfig::from_toml("claim_ttl_minutes = 0", defaults()).is_err());
        let config = BoardConfig::from_toml(
            "user = 'josh'\nhost = 'laptop'\nclaim_ttl_minutes = 90",
            defaults(),
        )
        .unwrap();
        assert_eq!(config.claim_ttl_seconds(), 5400);
        assert_eq!(
            config.actor(Some("codex"), Some("c1")).unwrap().identity(),
            "josh@laptop/codex/c1"
        );
        assert_eq!(config.actor(None, None).unwrap().harness.as_str(), "cli");
    }

    #[test]
    fn remote_mode_is_preserved_but_explicitly_unsupported() {
        let config = BoardConfig::from_toml(
            "mode = 'remote'\nurl = 'https://board.example'",
            BoardConfig::for_database("target/board.sqlite3"),
        )
        .unwrap();
        assert_eq!(config.mode, BoardMode::Remote);
        assert!(
            config
                .ensure_local()
                .unwrap_err()
                .to_string()
                .starts_with("board_remote_unsupported:")
        );
    }

    #[cfg(unix)]
    #[test]
    fn os_identity_uses_a_passwd_home_and_captured_hostname() {
        let account = passwd_account().unwrap();
        assert!(account.home.is_absolute());
        assert!(!account.user.is_empty());
        assert!(!machine_hostname().unwrap().is_empty());
    }
}
