//! `--no-daemon` member indexing within the query's maintenance slice; an
//! existing own or parent view answers when the slice ends first.
use crate::{
    daemon::deadline::{QueryDeadline, TIMED_OUT, is_timed_out},
    store::{Store, is_index_cache_unavailable, is_index_warming},
    workspace::{
        home_index::{HomeIndexPolicy, HomeIndexSource, ParentIndexView, resolve_home_index},
        member_root::MemberRoot,
    },
};
use anyhow::{Result, bail};

pub(super) fn inline_member_index(
    member: &MemberRoot,
    member_cache: &std::path::Path,
    explicit_cache: Option<&std::path::Path>,
    deadline: &QueryDeadline,
    maintenance: QueryDeadline,
) -> Result<HomeIndexSource> {
    let mut own_view = None;
    let mut parent_view = None;
    match resolve_home_index(
        member,
        member_cache,
        explicit_cache,
        HomeIndexPolicy::AwaitDaemon(std::time::Duration::ZERO),
        deadline.clone(),
    ) {
        HomeIndexSource::Own(store) => own_view = Some(store),
        HomeIndexSource::Parent(view) => parent_view = Some(view),
        HomeIndexSource::Warming { .. } => {}
        HomeIndexSource::Unavailable { reason } => bail!(reason),
    }

    if deadline.expired() {
        bail!("{TIMED_OUT}: query deadline expired");
    }
    if own_view.is_none()
        && let Some(parent) = parent_view.as_ref()
    {
        let _ = crate::store::ensure_seeded_within(
            parent.store.index_cache(),
            &member.root,
            member_cache,
            &maintenance,
            deadline,
        );
        if deadline.expired() {
            bail!("{TIMED_OUT}: query deadline expired");
        }
    }
    if maintenance.expired() {
        return available_home_or_warming(
            own_view,
            parent_view,
            "inline index preparation remains; run `trufflepig index`",
        );
    }

    let mut writer =
        match Store::open_for_reconcile(&member.root, member_cache, maintenance.clone()) {
            Ok(writer) => writer,
            Err(error) if maintenance_slice_ended(&maintenance, deadline, &error) => {
                return available_home_or_warming(
                    own_view,
                    parent_view,
                    "inline index preparation remains; run `trufflepig index`",
                );
            }
            Err(error) if is_index_warming(&error) || is_index_cache_unavailable(&error) => {
                return available_home_or_warming(
                    own_view,
                    parent_view,
                    "inline index preparation remains; run `trufflepig index`",
                );
            }
            Err(error) => return Err(error),
        };
    let attempt = if own_view.is_some() || parent_view.is_some() {
        writer.index_available_until(maintenance.clone())
    } else {
        writer.index_until(maintenance.clone())
    };
    match attempt {
        Ok(Some(_)) => Ok(HomeIndexSource::Own(Store::open_read(
            &member.root,
            member_cache,
            deadline.clone(),
        )?)),
        Ok(None) => available_home_or_warming(
            own_view,
            parent_view,
            "inline index preparation remains; run `trufflepig index`",
        ),
        Err(error) if maintenance_slice_ended(&maintenance, deadline, &error) => {
            available_home_or_warming(
                own_view,
                parent_view,
                "inline index preparation remains; run `trufflepig index`",
            )
        }
        Err(error) if is_index_warming(&error) => available_home_or_warming(
            own_view,
            parent_view,
            "inline index writer is busy; run `trufflepig index`",
        ),
        Err(error) => Err(error),
    }
}

fn available_home_or_warming(
    own_view: Option<Store>,
    parent_view: Option<ParentIndexView>,
    reason: &'static str,
) -> Result<HomeIndexSource> {
    if let Some(store) = own_view {
        Ok(HomeIndexSource::Own(store))
    } else if let Some(view) = parent_view {
        Ok(HomeIndexSource::Parent(view))
    } else {
        Ok(HomeIndexSource::Warming {
            reason: Some(reason),
        })
    }
}

fn maintenance_slice_ended(
    maintenance: &QueryDeadline,
    original: &QueryDeadline,
    error: &anyhow::Error,
) -> bool {
    is_timed_out(error) && !original.expired() && maintenance.expired()
}
