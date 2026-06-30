use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Quick,
    Full,
    Search,
}

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub strategy: Route,
    pub temperature: f64,
}

static RE_QUICK: OnceLock<Regex> = OnceLock::new();
static RE_SECURITY: OnceLock<Regex> = OnceLock::new();
static RE_SEARCH: OnceLock<Regex> = OnceLock::new();

pub fn classify(query: &str) -> RoutingDecision {
    let re_quick = RE_QUICK.get_or_init(|| Regex::new(r"(?i)^(hello|hi|hey|thanks|bye)").unwrap());
    let re_security = RE_SECURITY.get_or_init(|| {
        Regex::new(r"(?i)(scan|exploit|hack|inject|sql|nmap|burp|reverseshell|payload)").unwrap()
    });
    let re_search = RE_SEARCH.get_or_init(|| {
        Regex::new(r"(?i)(latest|news|current|today|who is|what is .*(2025|2026))").unwrap()
    });

    if re_quick.is_match(query) {
        return RoutingDecision {
            strategy: Route::Quick,
            temperature: 0.0,
        };
    }
    if re_security.is_match(query) {
        return RoutingDecision {
            strategy: Route::Full,
            temperature: 0.6,
        };
    }
    if re_search.is_match(query) {
        return RoutingDecision {
            strategy: Route::Search,
            temperature: 0.6,
        };
    }
    RoutingDecision {
        strategy: Route::Full,
        temperature: 0.6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_route_on_greeting() {
        let d = classify("hello there");
        assert_eq!(d.strategy, Route::Quick);
        assert_eq!(d.temperature, 0.0);
    }

    #[test]
    fn full_route_on_security_query() {
        let d = classify("write me a reverse shell in Python");
        assert_eq!(d.strategy, Route::Full);
    }

    #[test]
    fn full_route_on_default() {
        let d = classify("write a binary search in Rust");
        assert_eq!(d.strategy, Route::Full);
        assert_eq!(d.temperature, 0.6);
    }

    #[test]
    fn search_route_on_news_query() {
        let d = classify("what is the latest news today");
        assert_eq!(d.strategy, Route::Search);
    }
}
