mod hierarchy_index;
mod parent_lookup;
mod parent_projection;
mod php_names;
mod php_parents;
mod xenforo;

#[cfg(test)]
mod chain_projection_tests;
#[cfg(test)]
mod tests;

use crate::store::inheritance::DerivedBudget;
use anyhow::Result;
use rusqlite::Connection;

pub(crate) const SHARED_RESOLVER_REVISION: &str = "php-xenforo-inheritance-v1";

pub(super) fn resolve(conn: &mut Connection) -> Result<()> {
    let classes = php_names::class_index(conn)?;
    php_names::resolve(conn, &classes)?;

    let aliases = xenforo::load_alias_source(conn)?;
    let mut hierarchy = php_parents::build_index(conn, |name| {
        xenforo::canonicalize_class_name(name, &aliases)
    })?;
    let mut budget = DerivedBudget::new();
    php_parents::project(conn, &hierarchy, &mut budget)?;
    xenforo::resolve(conn, &classes, &mut hierarchy, &aliases, &mut budget)?;
    parent_projection::resolve_parent_calls(conn, &hierarchy, &mut budget)?;
    conn.execute(
        "DELETE FROM relationships WHERE kind=?1",
        [crate::extract::php_markers::MARKER_KIND],
    )?;
    Ok(())
}
