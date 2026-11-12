//! Channel checks through gateway routes, and the gateway revision poller.
//!
//! A check asks the route's gateway (`/v1/check`) about one channel's source; lineup only stores
//! the result and its transitions. Checks run one at a time and runs never overlap; the gateway
//! decides how to check safely (for Acestream: its activity guard, its own probe sessions). A
//! gateway that fails stops the run for that route: the rest of its channels are reported, not
//! recorded.
//!
//! The revision poller reads each enabled gateway route's `/v1/info` and resyncs the route when
//! its rule's match or play revision differs from the last complete sync.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use api_types::{CheckOutcome, CheckResult, ResyncTrigger, RouteConfig};
use gateway_contract::{Capability, RuleInfo};
use serde_json::json;

use crate::catalog::{Catalog, content_state};
use crate::error::{Result, conflict};
use crate::store::{ChannelFilter, CheckIdentity, CheckRecord, RouteRow};

#[derive(Debug, Clone, Copy)]
pub struct CheckTiming {
    /// Delay before the first scheduled run.
    pub first_run: Duration,
    /// Retry delay after a run in which channels were skipped (busy) or a gateway failed.
    pub busy_retry: Duration,
    /// How often the scheduler checks whether a run is due.
    pub tick: Duration,
    /// Delay before the first revision poll.
    pub first_poll: Duration,
    /// Time between revision polls.
    pub poll_every: Duration,
}

impl Default for CheckTiming {
    fn default() -> Self {
        CheckTiming {
            first_run: Duration::from_secs(600),
            busy_retry: Duration::from_secs(1800),
            tick: Duration::from_secs(60),
            first_poll: Duration::from_secs(60),
            poll_every: Duration::from_secs(600),
        }
    }
}

/// A channel to check, as it was when the check was requested.
struct Target {
    who: CheckIdentity,
    source: String,
}

pub struct Checker {
    catalog: Catalog,
    timing: CheckTiming,
    run_lock: tokio::sync::Mutex<()>,
}

impl Checker {
    pub fn new(catalog: Catalog, timing: CheckTiming) -> Arc<Self> {
        Arc::new(Checker {
            catalog,
            timing,
            run_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Check the given channels now and return the results. Each must be on a gateway route whose
    /// rule supports checks.
    pub async fn check(self: &Arc<Self>, ids: &[i64], force: bool) -> Result<Vec<CheckResult>> {
        // Each route's rule must support checks; ask its gateway once per route.
        let mut checkable: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut targets = Vec::new();
        for id in ids {
            let c = self.catalog.store.get_channel(*id).await?;
            let route = self.catalog.store.route_row(c.route_id).await?;
            if !checkable.contains(&route.id) {
                checkable_rule(&self.catalog, &route).await?;
                checkable.insert(route.id);
            }
            targets.push(self.target(&route, c.id).await?);
        }
        Ok(self.run_owned(targets, force).await.0)
    }

    /// All enabled channels of enabled gateway routes whose rule supports checks. Returns whether
    /// anything was skipped or a gateway failed (the scheduler then retries sooner).
    pub async fn check_all(self: &Arc<Self>, force: bool) -> Result<(Vec<CheckResult>, bool)> {
        let mut targets = Vec::new();
        let mut incomplete = false;
        for route in self.catalog.store.list_routes().await? {
            if !route.enabled || matches!(route.config, RouteConfig::Direct {}) || route.channel_count == 0 {
                continue;
            }
            let row = self.catalog.store.route_row(route.id).await?;
            match self.catalog.rule_info(&row).await {
                Ok(Some(rule)) if rule.can(Capability::Check) => {}
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!("checks of route {:?} skipped: {e}", row.name);
                    incomplete = true;
                    continue;
                }
            }
            let channels = self
                .catalog
                .store
                .list_channels(&ChannelFilter {
                    route_id: Some(row.id),
                    ..Default::default()
                })
                .await?;
            for c in channels.into_iter().filter(|c| c.enabled) {
                targets.push(self.target(&row, c.id).await?);
            }
        }
        let (results, stopped) = self.run_owned(targets, force).await;
        Ok((results, stopped || incomplete))
    }

    async fn target(&self, route: &RouteRow, channel_id: i64) -> Result<Target> {
        let s = self.catalog.store.channel_source(channel_id).await?;
        Ok(Target {
            who: CheckIdentity {
                channel_id,
                created_at: s.created_at,
                source_revision: s.source_revision,
                route_id: route.id,
                route_revision: route.revision,
            },
            source: s.source,
        })
    }

    /// Start a run over all channels in the background.
    pub fn spawn_all(self: &Arc<Self>, force: bool) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.check_all(force).await {
                tracing::warn!("check run: {e}");
            }
        });
    }

    /// Run in a task of its own: once started, a run finishes and keeps holding the run lock even
    /// if the caller goes away, so runs never overlap.
    async fn run_owned(self: &Arc<Self>, targets: Vec<Target>, force: bool) -> (Vec<CheckResult>, bool) {
        let me = self.clone();
        match tokio::spawn(async move { me.run(targets, force).await }).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("check run failed: {e}");
                (vec![], true)
            }
        }
    }

    async fn run(self: &Arc<Self>, targets: Vec<Target>, force: bool) -> (Vec<CheckResult>, bool) {
        let _lock = self.run_lock.lock().await;
        let mut results = Vec::new();
        // Routes whose gateway failed during this run, with the reason.
        let mut failed_routes: HashMap<i64, String> = HashMap::new();
        let mut incomplete = false;
        tracing::info!(
            "check run starting: {} channel(s){}",
            targets.len(),
            if force { ", forced" } else { "" }
        );
        for t in targets {
            let not_recorded = |outcome: CheckOutcome, reason: &str| CheckResult {
                channel_id: t.who.channel_id,
                outcome,
                reason: Some(reason.to_string()),
                content_state: None,
                detail: None,
                recorded: false,
            };
            if let Some(why) = failed_routes.get(&t.who.route_id) {
                results.push(not_recorded(CheckOutcome::Skipped, why));
                continue;
            }
            // The target may have waited for the lock: check only the source and route it named.
            let route = match self.catalog.store.route_row(t.who.route_id).await {
                Ok(r) if r.revision == t.who.route_revision => r,
                _ => {
                    results.push(not_recorded(CheckOutcome::Skipped, "the route changed"));
                    continue;
                }
            };
            let RouteConfig::Gateway { base_url, rule } = &route.config else {
                results.push(not_recorded(CheckOutcome::Skipped, "the route is direct"));
                continue;
            };
            match self.catalog.gateway.check(base_url, rule, &t.source, force).await {
                Ok(r) => {
                    let rec = CheckRecord {
                        outcome: outcome(r.outcome),
                        content_state: r.content_state.map(content_state),
                        reason: r.reason.clone(),
                        detail: r.detail.clone(),
                    };
                    if rec.outcome == CheckOutcome::Skipped {
                        incomplete = true;
                    }
                    tracing::info!(
                        "check of channel {}: {:?}{}",
                        t.who.channel_id,
                        rec.outcome,
                        rec.reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()
                    );
                    let mut result = CheckResult {
                        channel_id: t.who.channel_id,
                        outcome: rec.outcome,
                        reason: rec.reason.clone(),
                        content_state: rec.content_state,
                        detail: rec.detail.clone(),
                        recorded: false,
                    };
                    match self.catalog.store.record_check(t.who, rec).await {
                        Ok(recorded) => result.recorded = recorded,
                        Err(e) => tracing::warn!("recording check of channel {}: {e}", t.who.channel_id),
                    }
                    results.push(result);
                }
                Err(e) => {
                    let why = e.to_string();
                    tracing::warn!("check of channel {}: {why}", t.who.channel_id);
                    // The gateway failed, not the channel: unknown, and stop this route's run.
                    let rec = CheckRecord {
                        outcome: CheckOutcome::Unknown,
                        content_state: None,
                        reason: Some(e.code().to_string()),
                        detail: Some(json!({ "error": why })),
                    };
                    let recorded = self.catalog.store.record_check(t.who, rec).await.unwrap_or(false);
                    results.push(CheckResult {
                        channel_id: t.who.channel_id,
                        outcome: CheckOutcome::Unknown,
                        reason: Some(e.code().to_string()),
                        content_state: None,
                        detail: Some(json!({ "error": why })),
                        recorded,
                    });
                    failed_routes.insert(t.who.route_id, format!("the route's gateway failed: {why}"));
                    incomplete = true;
                }
            }
        }
        let n = |o: CheckOutcome| results.iter().filter(|r| r.outcome == o).count();
        tracing::info!(
            "check run done: {} ok, {} dead, {} unknown, {} skipped",
            n(CheckOutcome::Ok),
            n(CheckOutcome::Dead),
            n(CheckOutcome::Unknown),
            n(CheckOutcome::Skipped),
        );
        (results, incomplete)
    }

    /// Boot-relative schedule over all channels; off while `check_interval_hours` is 0.
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(me.timing.first_run).await;
            let mut next_due: Option<SystemTime> = None; // None: due now
            loop {
                let hours = me
                    .catalog
                    .store
                    .settings()
                    .await
                    .map(|s| s.check_interval_hours)
                    .unwrap_or(0);
                if hours > 0 && next_due.is_none_or(|t| SystemTime::now() >= t) {
                    match me.check_all(false).await {
                        Ok((_, true)) => {
                            tracing::info!(
                                "check run incomplete (skipped channels or a gateway failed); retrying later"
                            );
                            next_due = Some(SystemTime::now() + me.timing.busy_retry);
                        }
                        Ok(_) => next_due = Some(SystemTime::now() + Duration::from_secs(u64::from(hours) * 3600)),
                        Err(e) => {
                            tracing::warn!("scheduled check run: {e}");
                            next_due = Some(SystemTime::now() + me.timing.busy_retry);
                        }
                    }
                }
                tokio::time::sleep(me.timing.tick).await;
            }
        });
    }

    /// Poll every enabled gateway route's revisions and resync routes whose rule changed.
    pub fn spawn_revision_poller(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(me.timing.first_poll).await;
            loop {
                me.poll_revisions().await;
                tokio::time::sleep(me.timing.poll_every).await;
            }
        });
    }

    /// One poll over all enabled gateway routes (also used by tests).
    pub async fn poll_revisions(&self) {
        let routes = match self.catalog.store.list_routes().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("revision poll: {e}");
                return;
            }
        };
        for route in routes.into_iter().filter(|r| r.enabled) {
            let Ok(row) = self.catalog.store.route_row(route.id).await else {
                continue;
            };
            let rule = match self.catalog.rule_info(&row).await {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    tracing::debug!("revision poll of route {:?}: {e}", row.name);
                    continue;
                }
            };
            let current = row.synced_match_revision.as_deref() == Some(rule.match_revision.as_str())
                && row.synced_play_revision.as_deref() == Some(rule.play_revision.as_str());
            if current {
                continue;
            }
            tracing::info!(
                "route {:?}: gateway rule revisions changed (match {}, play {}); resyncing",
                row.name,
                rule.match_revision,
                rule.play_revision
            );
            match self.catalog.resync(row.id, ResyncTrigger::RevisionChange).await {
                Ok((_, report)) => tracing::info!(
                    "route {:?} resynced: {} updated, {} unchanged, {} failed, {} skipped",
                    row.name,
                    report.updated,
                    report.unchanged,
                    report.failed.len(),
                    report.skipped.len()
                ),
                Err(e) => tracing::warn!("resync of route {:?}: {e}", row.name),
            }
        }
    }
}

/// The route's rule, if it supports checks; otherwise an error naming why not.
async fn checkable_rule(catalog: &Catalog, route: &RouteRow) -> Result<RuleInfo> {
    match catalog.rule_info(route).await? {
        Some(r) if r.can(Capability::Check) => Ok(r),
        Some(r) => Err(conflict(format!(
            "route {:?}: the gateway's rule {:?} has no checks",
            route.name, r.name
        ))),
        None => Err(conflict(format!("route {:?} is direct: it has no checks", route.name))),
    }
}

fn outcome(o: gateway_contract::CheckOutcome) -> CheckOutcome {
    match o {
        gateway_contract::CheckOutcome::Ok => CheckOutcome::Ok,
        gateway_contract::CheckOutcome::Dead => CheckOutcome::Dead,
        gateway_contract::CheckOutcome::Unknown => CheckOutcome::Unknown,
        gateway_contract::CheckOutcome::Skipped => CheckOutcome::Skipped,
    }
}
