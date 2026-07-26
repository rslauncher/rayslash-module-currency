#[allow(warnings)]
mod bindings;

use bindings::exports::rayslash::module::provider::Guest;
use bindings::rayslash::module::{
    host,
    types::{Action, Icon, ModuleError, QueryContext, QueryResponse, ResultItem},
};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

struct Component;

#[derive(Deserialize, Serialize)]
struct Rate {
    rate: f64,
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
    let key = format!("rate-{base}-{quote}.json");
    if let Ok(Some(bytes)) = host::cache_get(&key)
        && let Ok(rate) = serde_json::from_slice::<Rate>(&bytes)
    {
        return valid_rate(rate.rate);
    }
    let request = host::HttpRequest {
        method: "GET".into(),
        url: format!("https://api.frankfurter.dev/v1/latest?base={base}&symbols={quote}"),
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
    let rate = parsed
        .rates
        .get(quote)
        .copied()
        .ok_or_else(|| "Currency service did not return the requested rate.".to_owned())
        .and_then(valid_rate)?;
    if let Ok(bytes) = serde_json::to_vec(&Rate { rate }) {
        let _ = host::cache_put(&key, &bytes);
    }
    Ok(rate)
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
}
