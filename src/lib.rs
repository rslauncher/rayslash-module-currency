#[allow(warnings)]
mod bindings;

use bindings::exports::rayslash::module::provider::Guest;
use bindings::rayslash::module::{
    host,
    types::{Action, Icon, ModuleError, QueryContext, QueryResponse, ResultItem},
};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const RATE_CACHE_TTL_SECONDS: u64 = 6 * 60 * 60;

struct Component;

#[derive(Clone, Deserialize, Serialize)]
struct CachedRates {
    rates: BTreeMap<String, f64>,
    #[serde(default)]
    fetched_at: u64,
}

#[derive(Deserialize)]
struct LatestRates {
    rates: BTreeMap<String, f64>,
}

impl Guest for Component {
    fn query(context: QueryContext) -> Result<QueryResponse, ModuleError> {
        let Some((amount, base, quote)) = parse(&context.query) else {
            return Ok(QueryResponse {
                results: Vec::new(),
                exclusive: false,
            });
        };
        let result = if base == quote {
            Ok(amount)
        } else {
            rate(&base, &quote).map(|rate| amount * rate)
        };
        let (title, action) = match result {
            Ok(value) => {
                let text = format!("{} {quote}", format_number(value));
                (text.clone(), Action::CopyText(text))
            }
            Err(error) => (error.clone(), Action::ShowMessage(error)),
        };
        Ok(QueryResponse {
            results: vec![ResultItem {
                id: format!("currency:{}:{base}:{quote}", format_number(amount)),
                title,
                subtitle: format!(
                    "Convert {} {base} to {quote} with Frankfurter",
                    format_number(amount)
                ),
                icon: Icon::PackagePath("icon.svg".into()),
                score: None,
                action,
            }],
            exclusive: true,
        })
    }
}

fn parse(input: &str) -> Option<(f64, String, String)> {
    let words = input.split_whitespace().collect::<Vec<_>>();
    let (amount, base, quote) = match words.as_slice() {
        [amount, base, separator, quote] if separator.eq_ignore_ascii_case("to") => (
            amount.replace(',', "").parse::<f64>().ok()?,
            code(base)?,
            code(quote)?,
        ),
        [amount_and_base, separator, quote] if separator.eq_ignore_ascii_case("to") => {
            let split = amount_and_base
                .char_indices()
                .rev()
                .take_while(|(_, character)| character.is_ascii_alphabetic())
                .last()
                .map(|(index, _)| index)?;
            let (amount, base) = amount_and_base.split_at(split);
            (
                amount.replace(',', "").parse::<f64>().ok()?,
                code(base)?,
                code(quote)?,
            )
        }
        _ => return None,
    };
    amount.is_finite().then_some((amount, base, quote))
}
fn code(value: &str) -> Option<String> {
    let value = value.to_ascii_uppercase();
    (value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())).then_some(value)
}
fn rate(base: &str, quote: &str) -> Result<f64, String> {
    let key = format!("rates-v2-{base}.json");
    let now = host::unix_time();
    let cached = host::cache_get(&key)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<CachedRates>(&bytes).ok());
    if let Some(rates) = cached.as_ref()
        && cache_is_fresh(rates.fetched_at, now)
        && let Some(rate) = rates.rates.get(quote).copied()
    {
        return valid_rate(rate);
    }
    match request_rates(base) {
        Ok(rates) => {
            let rate = rates
                .get(quote)
                .copied()
                .ok_or_else(|| "Currency service did not return the requested rate.".to_owned())
                .and_then(valid_rate)?;
            if let Ok(bytes) = serde_json::to_vec(&CachedRates {
                rates,
                fetched_at: now,
            }) {
                let _ = host::cache_put(&key, &bytes);
            }
            Ok(rate)
        }
        Err(error) => cached
            .and_then(|rates| rates.rates.get(quote).copied())
            .map(valid_rate)
            .transpose()
            .and_then(|rate| rate.ok_or(error)),
    }
}

fn cache_is_fresh(fetched_at: u64, now: u64) -> bool {
    fetched_at != 0 && now.saturating_sub(fetched_at) <= RATE_CACHE_TTL_SECONDS
}

fn request_rates(base: &str) -> Result<BTreeMap<String, f64>, String> {
    let request = host::HttpRequest {
        method: "GET".into(),
        url: format!("https://api.frankfurter.dev/v1/latest?base={base}"),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let response =
        host::request(&request).map_err(|error| format!("Currency rate unavailable: {error:?}"))?;
    if response.status != 200 {
        return Err(format!(
            "Currency service returned HTTP {}.",
            response.status
        ));
    }
    let parsed: LatestRates = serde_json::from_slice(&response.body)
        .map_err(|_| "Currency service returned invalid data.".to_owned())?;
    if parsed.rates.is_empty()
        || parsed
            .rates
            .values()
            .copied()
            .any(|rate| valid_rate(rate).is_err())
    {
        return Err("Currency service returned invalid rates.".into());
    }
    Ok(parsed.rates)
}
fn valid_rate(rate: f64) -> Result<f64, String> {
    (rate.is_finite() && rate > 0.0)
        .then_some(rate)
        .ok_or_else(|| "Currency service returned an invalid rate.".into())
}
fn format_number(value: f64) -> String {
    if (value - value.round()).abs() < 1e-10 {
        format!("{value:.0}")
    } else {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_currency_query() {
        assert_eq!(
            parse("25 brl to usd"),
            Some((25.0, "BRL".into(), "USD".into()))
        );
        assert_eq!(
            parse("10usd to brl"),
            Some((10.0, "USD".into(), "BRL".into()))
        );
        assert_eq!(
            parse("1,250.50EUR to USD"),
            Some((1250.5, "EUR".into(), "USD".into()))
        );
    }
    #[test]
    fn rejects_non_codes() {
        assert!(parse("25 dollars to usd").is_none());
    }
    #[test]
    fn parses_current_frankfurter_response() {
        let response: LatestRates = serde_json::from_str(
            r#"{"amount":1.0,"base":"BRL","date":"2026-07-10","rates":{"USD":0.19537}}"#,
        )
        .unwrap();
        assert_eq!(response.rates.get("USD"), Some(&0.19537));
    }

    #[test]
    fn currency_cache_expires_without_underflowing_future_timestamps() {
        assert!(cache_is_fresh(100, 100 + RATE_CACHE_TTL_SECONDS));
        assert!(!cache_is_fresh(100, 101 + RATE_CACHE_TTL_SECONDS));
        assert!(!cache_is_fresh(0, 1));
        assert!(cache_is_fresh(200, 100));
    }

    #[test]
    fn per_base_cache_serves_multiple_quotes() {
        let cached = CachedRates {
            rates: BTreeMap::from([("EUR".into(), 0.9), ("BRL".into(), 5.4)]),
            fetched_at: 100,
        };
        let encoded = serde_json::to_vec(&cached).unwrap();
        let decoded: CachedRates = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.rates.get("EUR"), Some(&0.9));
        assert_eq!(decoded.rates.get("BRL"), Some(&5.4));
    }
}
