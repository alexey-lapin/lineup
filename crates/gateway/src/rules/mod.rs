//! Rules: the named, configured instances of the kinds of references this gateway understands.
//! The gateway keeps them in a name → rule registry and knows nothing else about them.

pub mod acestream;
pub mod link;
pub mod page;

use axum::response::Response;
use futures_util::future::BoxFuture;
use gateway_contract::{CheckResponse, MatchResult, Observation, RuleInfo};

pub trait Rule: Send + Sync {
    fn info(&self) -> RuleInfo;
    fn match_inputs<'a>(&'a self, inputs: &'a [String]) -> BoxFuture<'a, Vec<MatchResult>>;
    /// Only called when `info` advertises observe.
    fn observe<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Observation>;
    /// Only called when `info` advertises check.
    fn check<'a>(&'a self, reference: &'a str, force: bool) -> BoxFuture<'a, CheckResponse>;
    /// `GET /play/<rule>.m3u8?ref=<ref>`, for rules the gateway plays itself.
    fn play<'a>(&'a self, _reference: &'a str, _head: bool) -> Option<BoxFuture<'a, Response>> {
        None
    }
}
