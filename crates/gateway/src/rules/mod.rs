//! Rules: the named, configured kinds of references this gateway understands.

pub mod acestream;
pub mod page;
pub mod vk;

use gateway_contract::RuleInfo;

/// A configured rule instance.
pub enum Rule {
    Acestream(acestream::Acestream),
    Page(page::PageRule),
}

impl Rule {
    pub fn info(&self) -> RuleInfo {
        match self {
            Rule::Acestream(a) => a.info(),
            Rule::Page(p) => p.info(),
        }
    }
}
