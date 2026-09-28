mod php_names;
mod xenforo;

#[cfg(test)]
mod tests;

use anyhow::Result;
use rusqlite::Connection;

pub(super) fn resolve(conn: &mut Connection) -> Result<()> {
    let classes = php_names::class_index(conn)?;
    php_names::resolve(conn, &classes)?;
    xenforo::resolve(conn, &classes)
}
