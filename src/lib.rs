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
struct LatestRate {
    base: String,
    quote: String,
    rate: f64,
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
        let (title, subtitle, action) = match result {
            Ok(value) => {
                let text = format!("{} {quote}", format_currency_amount(value));
                (
                    text.clone(),
                    format!(
                        "Convert {} {base} to {quote} with Frankfurter",
                        format_number(amount)
                    ),
                    Action::CopyText(text),
                )
            }
            Err(message) => {
                let subtitle = format!("{base} to {quote}. {message}");
                (
                    "Currency rate unavailable".to_owned(),
                    subtitle.clone(),
                    Action::ShowMessage(subtitle),
                )
            }
        };
        Ok(QueryResponse {
            results: vec![ResultItem {
                id: format!("currency:{}:{base}:{quote}", format_number(amount)),
                title,
                subtitle,
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
fn rate(base: &str, quote: &str) -> Result<f64, &'static str> {
    let key = format!("rates-v2-{base}.json");
    let now = host::unix_time();
    let cached = host::cache_get(&key)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<CachedRates>(&bytes).ok());
    let cached_rate = cached
        .as_ref()
        .and_then(|table| table.rates.get(quote).copied())
        .filter(|&rate| is_valid_rate(rate));
    if let Some(rate) = cached_rate
        && cached.is_some_and(|table| cache_is_fresh(table.fetched_at, now))
    {
        return Ok(rate);
    }
    match request_rates(base) {
        Ok(rates) => {
            let rate = rates
                .get(quote)
                .copied()
                .ok_or("Frankfurter has no rate for this pair.")?;
            if let Ok(bytes) = serde_json::to_vec(&CachedRates {
                rates,
                fetched_at: now,
            }) {
                let _ = host::cache_put(&key, &bytes);
            }
            Ok(rate)
        }
        Err(error) => cached_rate.ok_or(error),
    }
}

fn cache_is_fresh(fetched_at: u64, now: u64) -> bool {
    fetched_at != 0 && now.saturating_sub(fetched_at) <= RATE_CACHE_TTL_SECONDS
}

fn request_rates(base: &str) -> Result<BTreeMap<String, f64>, &'static str> {
    let request = host::HttpRequest {
        method: "GET".into(),
        url: format!("https://api.frankfurter.dev/v2/rates?base={base}"),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let unavailable = "Check the currency codes or try again later.";
    let response = host::request(&request).map_err(|_| unavailable)?;
    if response.status != 200 {
        return Err(unavailable);
    }
    parse_latest_rates(&response.body, base)
}

fn parse_latest_rates(body: &[u8], base: &str) -> Result<BTreeMap<String, f64>, &'static str> {
    const INVALID_RATES: &str = "Frankfurter returned invalid rates. Try again later.";

    let entries: Vec<LatestRate> = serde_json::from_slice(body).map_err(|_| INVALID_RATES)?;
    if entries.is_empty() {
        return Err("Frankfurter returned no rates. Check the currency codes.");
    }
    let mut rates = BTreeMap::new();
    for entry in entries {
        if entry.base != base
            || !is_valid_rate(entry.rate)
            || rates.insert(entry.quote, entry.rate).is_some()
        {
            return Err(INVALID_RATES);
        }
    }
    Ok(rates)
}
fn is_valid_rate(rate: f64) -> bool {
    rate.is_finite() && rate > 0.0
}

fn format_currency_amount(value: f64) -> String {
    let formatted = format!("{value:.2}");
    if formatted == "-0.00" {
        "0.00".into()
    } else {
        formatted
    }
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
    fn parses_argentine_peso_rates_from_frankfurter_v2() {
        let rates = parse_latest_rates(
            br#"[
                {"date":"2026-11-20","base":"ARS","quote":"BRL","rate":0.003775},
                {"date":"2026-11-20","base":"ARS","quote":"USD","rate":0.0006398}
            ]"#,
            "ARS",
        )
        .unwrap();
        assert_eq!(format_currency_amount(7400.0 * rates["BRL"]), "27.94");
        assert_eq!(rates["USD"], 0.0006398);
    }

    #[test]
    fn currency_results_keep_two_decimal_places() {
        assert_eq!(format_currency_amount(12.0), "12.00");
        assert_eq!(format_currency_amount(5.4), "5.40");
        assert_eq!(format_currency_amount(-5.436), "-5.44");
        assert_eq!(format_currency_amount(-0.001), "0.00");
    }

    #[test]
    fn rejects_wrong_base_invalid_rates_and_ambiguous_quotes() {
        for response in [
            r#"{"amount":1.0,"base":"ARS","rates":{"BRL":0.003775}}"#,
            r#"[]"#,
            r#"[{"base":"USD","quote":"BRL","rate":5.4}]"#,
            r#"[{"base":"ARS","quote":"BRL","rate":0}]"#,
            r#"[{"base":"ARS","quote":"BRL","rate":0.003775},{"base":"ARS","quote":"BRL","rate":0.004}]"#,
        ] {
            assert!(parse_latest_rates(response.as_bytes(), "ARS").is_err());
        }
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
