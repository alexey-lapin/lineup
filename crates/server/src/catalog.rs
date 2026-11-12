//! Operations that validate sources with a route and then write: channel create and edit, resolve
//! (preview), import, route edits and resyncs. Gateway calls happen first, without the store's write
//! lock; the store then refuses the write if the route or channel changed meanwhile.

use std::sync::Arc;

use api_types::{
    Channel, ChannelCreate, ChannelInfo, ChannelPatch, GatewaySummary, ImportReport, ResolveResponse, ResyncItem,
    ResyncReport, ResyncTrigger, Route, RouteConfig, RouteCreate, RoutePatch, RouteStatus, RouteTestRequest,
    RouteTestResult, RuleSummary, Suggestion,
};
use gateway_contract::{Capability, Info, RuleInfo};

use crate::error::{Result, invalid};
use crate::gateway::Gateway;
use crate::routes::{self, Outcome};
use crate::store::{
    ChannelUpdate, ImportBatch, ImportEntryIn, NewChannelRequest, ResyncPublish, ResyncRow, RouteRow, SourceChange,
    Store, check_base_change, new_report,
};
use crate::{m3u, suggest};

/// Best-match score from which the match's tvg-id and logo are suggested.
const SUGGEST_SCORE: f64 = 0.6;
const MAX_MATCHES: usize = 5;

#[derive(Clone)]
pub struct Catalog {
    pub store: Arc<Store>,
    pub gateway: Gateway,
}

impl Catalog {
    pub fn new(store: Arc<Store>, gateway: Gateway) -> Self {
        Catalog { store, gateway }
    }

    // ---------------------------------------------------------------- channels

    pub async fn create_channel(&self, req: ChannelCreate) -> Result<Channel> {
        let route = self.store.route_row(req.route_id).await?;
        let (validated, _) = routes::validate_one(&self.gateway, &route.config, &req.input).await?;
        self.store
            .create_channel(NewChannelRequest {
                name: req.name,
                group_title: req.group_title,
                tvg_id: req.tvg_id,
                tvg_logo: req.tvg_logo,
                route_id: route.id,
                route_revision: route.revision,
                validated,
                is_event: req.is_event,
                kickoff_at: req.kickoff_at,
                enabled: req.enabled,
                playlists: req.playlists,
            })
            .await
    }

    pub async fn patch_channel(&self, id: i64, patch: ChannelPatch) -> Result<Channel> {
        let source = if patch.route_id.is_some() || patch.input.is_some() {
            let cur = self.store.channel_source(id).await?;
            let route = self.store.route_row(patch.route_id.unwrap_or(cur.route_id)).await?;
            let input = patch.input.clone().unwrap_or_else(|| cur.source.clone());
            let (validated, _) = routes::validate_one(&self.gateway, &route.config, &input).await?;
            Some(SourceChange {
                route_id: route.id,
                route_revision: route.revision,
                validated,
                based_on_source_revision: cur.source_revision,
            })
        } else {
            None
        };
        self.store
            .patch_channel(
                id,
                ChannelUpdate {
                    name: patch.name,
                    group_title: patch.group_title,
                    tvg_id: patch.tvg_id,
                    tvg_logo: patch.tvg_logo,
                    is_event: patch.is_event,
                    kickoff_at: patch.kickoff_at,
                    enabled: patch.enabled,
                    source,
                },
            )
            .await
    }

    /// Validate an input with a route and suggest channel metadata (nothing is written).
    pub async fn resolve(&self, route_id: i64, input: &str) -> Result<ResolveResponse> {
        let route = self.store.route_row(route_id).await?;
        let (v, _) = routes::validate_one(&self.gateway, &route.config, input).await?;
        let duplicate = self.store.find_duplicate(route.id, &v.identity).await?;
        let mut suggestion = Suggestion {
            name: routes::default_name(&route.config, &v),
            group_title: v.hints.group.clone(),
            is_event: v.hints.event.unwrap_or(false),
            ..Default::default()
        };
        // A placeholder name matches nothing useful: rank guide channels only for a real title.
        let matches = match &v.hints.title {
            Some(title) => {
                let all = self.store.epg_channels().await?;
                suggest::rank(all, title, v.hints.region.as_deref(), MAX_MATCHES)
            }
            None => vec![],
        };
        if let Some(best) = matches.first().filter(|m| m.score.unwrap_or(0.0) >= SUGGEST_SCORE) {
            suggestion.tvg_id = Some(best.xmltv_id.clone());
            suggestion.tvg_logo = best.icon.clone();
        }
        Ok(ResolveResponse {
            route_id: route.id,
            validated: v.to_api(&route.config),
            duplicate_of: duplicate.as_ref().map(|d| d.0),
            duplicate_source: duplicate.map(|d| d.1).filter(|s| *s != v.source),
            suggestion,
            matches,
        })
    }

    // ---------------------------------------------------------------- import

    /// Import an m3u into playlist `slug`, validating every entry with `route_id`. A gateway failure
    /// aborts the import with nothing written; entries the route does not accept are skipped.
    pub async fn import(&self, text: &str, slug: &str, route_id: i64, dry_run: bool) -> Result<ImportReport> {
        let route = self.store.route_row(route_id).await?;
        let parsed = m3u::parse(text);
        let mut skipped_lines = parsed.skipped;
        let inputs: Vec<String> = parsed.entries.iter().map(|e| e.url.clone()).collect();
        let batch = routes::validate(&self.gateway, &route.config, &inputs).await?;
        let mut entries = Vec::new();
        for (i, (e, outcome)) in parsed.entries.iter().zip(batch.items).enumerate() {
            let name = if e.name.trim().is_empty() {
                e.attr("tvg-name").unwrap_or(&e.url).to_string()
            } else {
                e.name.trim().to_string()
            };
            match outcome {
                Outcome::Ok(validated) => entries.push(ImportEntryIn {
                    index: i + 1,
                    name,
                    group_title: e.attr("group-title").map(str::to_string),
                    tvg_id: e.attr("tvg-id").map(str::to_string),
                    tvg_logo: e.attr("tvg-logo").map(str::to_string),
                    validated,
                }),
                other => skipped_lines.push(format!(
                    "entry {} ({name}): {}",
                    i + 1,
                    other.reason().unwrap_or_default()
                )),
            }
        }
        self.store
            .import_validated(ImportBatch {
                slug: slug.to_string(),
                dry_run,
                route_id: route.id,
                route_revision: route.revision,
                entries,
                skipped_lines,
            })
            .await
    }

    // ---------------------------------------------------------------- routes

    pub async fn create_route(&self, req: RouteCreate) -> Result<Route> {
        self.store.create_route(&req.name, req.config, req.enabled).await
    }

    pub async fn patch_route(&self, id: i64, patch: RoutePatch) -> Result<Route> {
        if let Some(base) = &patch.base_url {
            let row = self.store.route_row(id).await?;
            check_base_change(&row.config)?;
            let base = routes::normalize_base(base)?;
            let RouteConfig::Gateway { base_url, rule } = &row.config else {
                unreachable!("checked above")
            };
            if *base_url != base {
                let config = RouteConfig::Gateway {
                    base_url: base.clone(),
                    rule: rule.clone(),
                };
                // Name and enabled go into the same transaction: all or nothing.
                let meta = (patch.name.clone(), patch.enabled);
                return Ok(self
                    .resync_with(&row, config, Some(base), ResyncTrigger::BaseUrlChange, meta)
                    .await?
                    .0);
            }
        }
        if patch.name.is_some() || patch.enabled.is_some() {
            return self.store.update_route(id, patch.name, patch.enabled).await;
        }
        self.store.get_route(id).await
    }

    /// Revalidate every channel of a gateway route and publish the results.
    pub async fn resync(&self, id: i64, trigger: ResyncTrigger) -> Result<(Route, ResyncReport)> {
        let row = self.store.route_row(id).await?;
        if matches!(row.config, RouteConfig::Direct {}) {
            return Err(invalid("a direct route has nothing to resync"));
        }
        let config = row.config.clone();
        self.resync_with(&row, config, None, trigger, (None, None)).await
    }

    async fn resync_with(
        &self,
        row: &RouteRow,
        config: RouteConfig,
        new_base: Option<String>,
        trigger: ResyncTrigger,
        (name, enabled): (Option<String>, Option<bool>),
    ) -> Result<(Route, ResyncReport)> {
        let channels = self.store.route_channel_sources(row.id).await?;
        let inputs: Vec<String> = channels.iter().map(|c| c.source.clone()).collect();
        // A base URL change revalidates against the new gateway even with no channels, so a wrong
        // address is refused.
        let batch = routes::validate(&self.gateway, &config, &inputs).await?;
        let (match_rev, play_rev) = batch
            .rule
            .as_ref()
            .map(|r| (Some(r.match_revision.clone()), Some(r.play_revision.clone())))
            .unwrap_or((None, None));
        let mut report = new_report(trigger, channels.len(), match_rev, play_rev);
        let mut rows = Vec::new();
        for (c, outcome) in channels.into_iter().zip(batch.items) {
            match outcome {
                Outcome::Ok(validated) => rows.push(ResyncRow {
                    channel_id: c.channel_id,
                    based_on: c.source_revision,
                    validated,
                }),
                other => report.failed.push(ResyncItem {
                    channel_id: c.channel_id,
                    name: c.name,
                    reason: other.reason().unwrap_or_default().to_string(),
                }),
            }
        }
        let all_or_nothing = new_base.is_some();
        self.store
            .publish_resync(ResyncPublish {
                route_id: row.id,
                expected_revision: row.revision,
                new_base,
                rows,
                report,
                all_or_nothing,
                name,
                enabled,
            })
            .await
    }

    /// Test a route config, and optionally an input with it, without saving anything.
    pub async fn test_route(&self, req: RouteTestRequest) -> Result<RouteTestResult> {
        let started = std::time::Instant::now();
        let config = routes::normalize_config(req.config)?;
        let mut result = RouteTestResult {
            ok: false,
            detail: String::new(),
            gateway: None,
            sample: None,
            elapsed_ms: 0,
        };
        if let RouteConfig::Gateway { base_url, rule } = &config {
            match self.gateway.info(base_url).await {
                Ok(info) => {
                    result.gateway = Some(summary(&info));
                    match info.rule(rule) {
                        Some(r) => {
                            result.ok = true;
                            result.detail = format!(
                                "{} {} answered; rule {:?} is available",
                                info.name, info.version, r.title
                            );
                        }
                        None => {
                            result.detail =
                                format!("{} {} answered, but it has no rule {rule:?}", info.name, info.version);
                        }
                    }
                }
                Err(e) => result.detail = e.to_string(),
            }
        } else {
            result.ok = true;
            result.detail = "direct routes play stream URLs as they are".into();
        }
        if result.ok
            && let Some(input) = req.input.as_deref().filter(|i| !i.trim().is_empty())
        {
            match routes::validate_one(&self.gateway, &config, input).await {
                Ok((v, _)) => result.sample = Some(v.to_api(&config)),
                Err(e) => {
                    result.ok = false;
                    result.detail = format!("{}; the input was not accepted: {e}", result.detail);
                }
            }
        }
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    /// The saved route's gateway, live.
    pub async fn route_status(&self, id: i64) -> Result<RouteStatus> {
        let started = std::time::Instant::now();
        let row = self.store.route_row(id).await?;
        let mut status = RouteStatus {
            route_id: id,
            ok: true,
            detail: "direct routes play stream URLs as they are".into(),
            gateway: None,
            rule: None,
            pending_channels: None,
            elapsed_ms: 0,
        };
        if let RouteConfig::Gateway { base_url, rule } = &row.config {
            status.ok = false;
            match self.gateway.info(base_url).await {
                Ok(info) => {
                    status.gateway = Some(summary(&info));
                    match info.rule(rule) {
                        Some(r) => {
                            status.ok = true;
                            status.detail = format!("{} {} answered", info.name, info.version);
                            status.pending_channels = Some(self.store.route_pending(id, &r.play_revision).await?);
                            status.rule = Some(rule_summary(r));
                        }
                        None => status.detail = format!("the gateway has no rule {rule:?} any more"),
                    }
                }
                Err(e) => status.detail = e.to_string(),
            }
        }
        status.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(status)
    }

    /// The route's gateway rule, when the route is a gateway route that answers.
    pub async fn rule_info(&self, row: &RouteRow) -> Result<Option<RuleInfo>> {
        match &row.config {
            RouteConfig::Direct {} => Ok(None),
            RouteConfig::Gateway { base_url, rule } => {
                let info = self.gateway.info(base_url).await?;
                Ok(Some(routes::find_rule(&info, base_url, rule)?.clone()))
            }
        }
    }

    // ---------------------------------------------------------------- details

    /// Stored health and, where the route can observe, what its gateway reports now.
    pub async fn channel_info(&self, id: i64) -> Result<ChannelInfo> {
        let channel = self.store.get_channel(id).await?;
        let row = self.store.route_row(channel.route_id).await?;
        let mut info = ChannelInfo {
            channel_id: id,
            route_id: row.id,
            checks: api_types::Availability::no("direct routes have no checks"),
            health: channel.health.clone(),
            observation: None,
            observation_error: None,
        };
        let RouteConfig::Gateway { base_url, rule } = &row.config else {
            return Ok(info);
        };
        let rule_info = match self.rule_info(&row).await {
            Ok(r) => r.expect("gateway route"),
            Err(e) => {
                info.checks = api_types::Availability::no(e.to_string());
                info.observation_error = Some(e.to_string());
                return Ok(info);
            }
        };
        info.checks = if rule_info.can(Capability::Check) {
            api_types::Availability::yes()
        } else {
            api_types::Availability::no(format!("the gateway's rule {rule:?} has no checks"))
        };
        if !rule_info.can(Capability::Observe) {
            return Ok(info);
        }
        match self
            .gateway
            .observe(base_url, rule, std::slice::from_ref(&channel.source))
            .await
        {
            Ok(mut obs) => {
                if let Some(o) = obs.pop() {
                    info.observation = Some(api_types::Observation {
                        available: o.available,
                        reason: o.reason,
                        content_state: o.content_state.map(content_state),
                        watching: o.watching,
                        facts: o
                            .facts
                            .into_iter()
                            .map(|f| api_types::Fact {
                                label: f.label,
                                value: f.value,
                            })
                            .collect(),
                        title: o.title,
                    });
                }
            }
            Err(e) => info.observation_error = Some(e.to_string()),
        }
        Ok(info)
    }
}

pub fn content_state(s: gateway_contract::ContentState) -> api_types::ContentState {
    match s {
        gateway_contract::ContentState::Live => api_types::ContentState::Live,
        gateway_contract::ContentState::EndedProcessing => api_types::ContentState::EndedProcessing,
        gateway_contract::ContentState::Recording => api_types::ContentState::Recording,
    }
}

fn summary(info: &Info) -> GatewaySummary {
    GatewaySummary {
        name: info.name.clone(),
        version: info.version.clone(),
        contract: info.contract,
        rules: info.rules.iter().map(rule_summary).collect(),
    }
}

fn rule_summary(r: &RuleInfo) -> RuleSummary {
    RuleSummary {
        name: r.name.clone(),
        title: r.title.clone(),
        input_hint: r.input_hint.clone(),
        capabilities: r
            .capabilities
            .iter()
            .filter_map(|c| match c {
                Capability::Observe => Some("observe".to_string()),
                Capability::Check => Some("check".to_string()),
                Capability::Unknown => None,
            })
            .collect(),
        match_revision: r.match_revision.clone(),
        play_revision: r.play_revision.clone(),
    }
}
