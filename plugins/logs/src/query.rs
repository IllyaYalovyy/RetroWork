//! The filter language of the records table.
//!
//! A query is whitespace-separated tokens:
//!
//! - `level:error` keeps records of that level. Several `level:` tokens widen
//!   the match (`level:error level:warn`).
//! - `component:auth` keeps records whose component contains the text.
//!   Several `component:` tokens widen the match.
//! - Any other token must appear in the message or the component.
//!
//! Matching ignores ASCII case. Different kinds of token narrow the result.

use crate::record::{Level, LogRecord};

/// A parsed filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    levels: Vec<Level>,
    components: Vec<String>,
    terms: Vec<String>,
    problem: Option<String>,
}

impl Query {
    /// Parses filter text. Never fails; see [`Self::problem`].
    pub fn parse(text: &str) -> Self {
        let mut query = Query::default();
        for token in text.split_whitespace() {
            if let Some(name) = token.strip_prefix("level:") {
                match Level::parse(name) {
                    Some(level) => query.levels.push(level),
                    None => {
                        query.problem.get_or_insert_with(|| {
                            format!("unknown level `{name}` (use error, warn, info, debug, trace)")
                        });
                    }
                }
            } else if let Some(component) = token.strip_prefix("component:") {
                if component.is_empty() {
                    query
                        .problem
                        .get_or_insert_with(|| "`component:` needs a name".to_owned());
                } else {
                    query.components.push(component.to_ascii_lowercase());
                }
            } else {
                query.terms.push(token.to_ascii_lowercase());
            }
        }
        query
    }

    /// Why this query cannot match anything, if it is malformed. A malformed
    /// query matches no record, so a typo never shows unfiltered data as if it
    /// were the answer.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether the query keeps every record.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
            && self.components.is_empty()
            && self.terms.is_empty()
            && self.problem.is_none()
    }

    /// Whether `record` passes the query.
    pub fn matches(&self, record: &LogRecord) -> bool {
        if self.problem.is_some() {
            return false;
        }
        if !self.levels.is_empty() && !self.levels.contains(&record.level) {
            return false;
        }
        let component = record.component.to_ascii_lowercase();
        if !self.components.is_empty()
            && !self
                .components
                .iter()
                .any(|wanted| component.contains(wanted))
        {
            return false;
        }
        if self.terms.is_empty() {
            return true;
        }
        let message = record.message.to_ascii_lowercase();
        self.terms
            .iter()
            .all(|term| message.contains(term) || component.contains(term))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::parse_log;

    const LOG: &str = "\
t1 INFO auth.session: user 42 signed in
t2 WARN auth.session: token expires soon
t3 ERROR auth.db: connection refused
t4 ERROR api.gateway: upstream Timeout for user 42
t5 DEBUG api.gateway: retry scheduled
a stack trace line
";

    fn matching(query: &str) -> Vec<usize> {
        let query = Query::parse(query);
        parse_log(LOG)
            .iter()
            .filter(|record| query.matches(record))
            .map(|record| record.line)
            .collect()
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(Query::parse("  ").is_empty());
        assert_eq!(matching(""), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn level_tokens_widen_each_other() {
        assert_eq!(matching("level:error"), [3, 4]);
        assert_eq!(matching("level:error level:WARNING"), [2, 3, 4]);
        assert_eq!(matching("level:unknown"), [6]);
    }

    #[test]
    fn component_and_terms_narrow_the_level_match() {
        assert_eq!(matching("component:auth"), [1, 2, 3]);
        assert_eq!(matching("level:error component:API"), [4]);
        assert_eq!(matching("component:db component:gateway"), [3, 4, 5]);
        assert_eq!(matching("user 42"), [1, 4]);
        assert_eq!(matching("timeout"), [4], "terms ignore case");
        assert_eq!(
            matching("gateway retry"),
            [5],
            "terms also search the component"
        );
        assert_eq!(matching("level:error user"), [4]);
        assert_eq!(matching("nothing-like-this"), Vec::<usize>::new());
    }

    #[test]
    fn malformed_query_matches_nothing_and_says_why() {
        let query = Query::parse("level:eror");
        assert!(query.problem().unwrap().contains("eror"));
        assert!(!query.is_empty());
        assert_eq!(matching("level:eror"), Vec::<usize>::new());
        assert_eq!(matching("level:error level:"), Vec::<usize>::new());
        assert!(Query::parse("component:").problem().is_some());
    }
}
