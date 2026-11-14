//! Model list prices and context windows, fetched from a public catalogue.
//!
//! The daemon fetches the list by itself (a plain GET that says nothing about
//! the user or the project), keeps the parsed copy under the state directory,
//! and fetches again once a day. A failed fetch keeps the old copy. No price
//! and no window size is ever written into this program: a model that is not
//! in the list has no price here.
//!
//! The catalogue is models.dev (`https://models.dev/api.json`): providers,
//! each with its models, each with `cost` in dollars per million tokens
//! (`input`, `output`, `cache_read`, `cache_write`) and `limit.context`.
//!
//! Matching a model named in a session file to the list:
//! 1. the id exactly as written, under the provider the session names
//!    (Claude Code: anthropic; Codex: `model_provider` in its session file;
//!    OpenCode: the `providerID` it stores);
//! 2. then the id normalised: lower case, a `[…]` suffix removed, a leading
//!    `provider/` removed, a trailing date (`-20251001`, `@20251001`,
//!    `-2025-10-01`) removed.
//!
//! Another provider's entry for a model of the same name is never used: a
//! reseller's price is not the price paid.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spandeck_archive::usage::Tally;

use crate::daemon::Daemon;
use crate::paths::now_ms;

pub const DEFAULT_URL: &str = "https://models.dev/api.json";
const DAY_MS: u64 = 24 * 3600 * 1000;
/// After a failed fetch, wait this long before trying again.
const RETRY_MS: u64 = 3600 * 1000;

/// Dollars per million tokens. A missing figure is unknown, not zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Price {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    /// The model's context window in tokens.
    pub context: Option<u64>,
}

impl Price {
    /// What `tally` costs at these prices, or `None` when tokens of some kind
    /// were used and the list has no price for that kind.
    pub fn cost(&self, tally: &Tally) -> Option<f64> {
        let part = |tokens: i64, price: Option<f64>| match (tokens, price) {
            (t, _) if t <= 0 => Some(0.0),
            (t, Some(p)) => Some(t as f64 * p / 1e6),
            _ => None,
        };
        Some(
            part(tally.input, self.input)?
                + part(tally.output, self.output)?
                + part(tally.cache_read, self.cache_read)?
                + part(tally.cache_write, self.cache_write)?,
        )
    }

    fn has_cost(&self) -> bool {
        self.input.is_some() || self.output.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PriceList {
    pub source: String,
    pub fetched_ms: u64,
    /// `provider/model` -> price.
    pub models: BTreeMap<String, Price>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PricesConfig {
    /// Fetch the public list. Default: on.
    pub auto: bool,
    /// Another list in the same shape. Default: models.dev.
    pub url: Option<String>,
    /// Figures that replace the list's, by model id or `provider/model`.
    pub model: BTreeMap<String, Price>,
}

impl Default for PricesConfig {
    fn default() -> Self {
        PricesConfig {
            auto: true,
            url: None,
            model: BTreeMap::new(),
        }
    }
}

/// The model id as the list would write it (step 2 of the matching rule).
pub fn normalise(model: &str) -> String {
    let mut m = model.trim().to_lowercase();
    if let Some(i) = m.find('[') {
        m.truncate(i);
    }
    if let Some(i) = m.rfind('/') {
        m = m[i + 1..].to_string();
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // -2025-10-01
    if m.len() > 11 {
        let (head, tail) = m.split_at(m.len() - 11);
        let t = tail.as_bytes();
        if t[0] == b'-'
            && t[5] == b'-'
            && t[8] == b'-'
            && digits(&tail[1..5])
            && digits(&tail[6..8])
            && digits(&tail[9..11])
        {
            return head.to_string();
        }
    }
    // -20251001 or @20251001
    if m.len() > 9 {
        let (head, tail) = m.split_at(m.len() - 9);
        if (tail.starts_with('-') || tail.starts_with('@')) && digits(&tail[1..]) {
            return head.to_string();
        }
    }
    m
}

impl PriceList {
    /// Read the catalogue's own shape.
    pub fn parse(catalogue: &Value, source: &str, fetched_ms: u64) -> anyhow::Result<PriceList> {
        let providers = catalogue
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("the price list is not an object of providers"))?;
        let mut models = BTreeMap::new();
        let num = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| *x >= 0.0);
        for (provider, p) in providers {
            let Some(list) = p.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (id, m) in list {
                let cost = m.get("cost").cloned().unwrap_or(Value::Null);
                let price = Price {
                    input: num(&cost, "input"),
                    output: num(&cost, "output"),
                    cache_read: num(&cost, "cache_read"),
                    cache_write: num(&cost, "cache_write"),
                    context: m.pointer("/limit/context").and_then(|c| c.as_u64()),
                };
                if price.has_cost() || price.context.is_some() {
                    models.insert(format!("{}/{}", provider.to_lowercase(), id), price);
                }
            }
        }
        if models.is_empty() {
            anyhow::bail!("the price list names no models");
        }
        Ok(PriceList {
            source: source.to_string(),
            fetched_ms,
            models,
        })
    }

    pub fn find(&self, provider: Option<&str>, model: &str) -> Option<&Price> {
        let provider = provider?.to_lowercase();
        self.models
            .get(&format!("{provider}/{model}"))
            .or_else(|| self.models.get(&format!("{provider}/{}", normalise(model))))
    }

    pub fn load(file: &Path) -> Option<PriceList> {
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
    }

    /// Fetch and parse the list. Nothing but the URL is sent.
    pub async fn fetch(url: &str) -> anyhow::Result<PriceList> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("spandeck")
            .build()?;
        let body: Value = http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        PriceList::parse(&body, url, now_ms())
    }

    /// Fetch the list and keep it at `file`. On any failure the file is left
    /// as it was.
    pub async fn refresh(url: &str, file: &Path) -> anyhow::Result<PriceList> {
        let list = PriceList::fetch(url).await?;
        let tmp = file.with_extension("json.new");
        std::fs::write(&tmp, serde_json::to_string(&list)?)?;
        std::fs::rename(&tmp, file)?;
        Ok(list)
    }
}

#[derive(Default)]
pub struct PricesState {
    list: Mutex<Option<Arc<PriceList>>>,
    last_try_ms: Mutex<u64>,
}

/// The prices in force: the fetched list with the owner's own figures on top.
pub struct Prices<'a> {
    pub list: Option<Arc<PriceList>>,
    cfg: &'a PricesConfig,
}

impl Prices<'_> {
    pub fn find(&self, provider: Option<&str>, model: &str) -> Option<Price> {
        let listed = self
            .list
            .as_ref()
            .and_then(|l| l.find(provider, model))
            .cloned();
        let own = provider
            .and_then(|p| self.cfg.model.get(&format!("{p}/{model}")))
            .or_else(|| self.cfg.model.get(model))
            .or_else(|| self.cfg.model.get(&normalise(model)));
        match (own, listed) {
            (Some(own), listed) => {
                let l = listed.unwrap_or_default();
                Some(Price {
                    input: own.input.or(l.input),
                    output: own.output.or(l.output),
                    cache_read: own.cache_read.or(l.cache_read),
                    cache_write: own.cache_write.or(l.cache_write),
                    context: own.context.or(l.context),
                })
            }
            (None, listed) => listed,
        }
    }

    pub fn cost(&self, provider: Option<&str>, model: &str, tally: &Tally) -> Option<f64> {
        self.find(provider, model)
            .filter(|p| p.has_cost())
            .and_then(|p| p.cost(tally))
    }

    pub fn source(&self) -> Option<&str> {
        self.list.as_ref().map(|l| l.source.as_str())
    }

    pub fn fetched_ms(&self) -> Option<u64> {
        self.list.as_ref().map(|l| l.fetched_ms)
    }
}

impl Daemon {
    pub fn prices(&self) -> Prices<'_> {
        Prices {
            list: self.prices.list.lock().unwrap().clone(),
            cfg: &self.cfg.prices,
        }
    }

    pub(crate) fn prices_load(&self) {
        if let Some(list) = PriceList::load(&self.paths.prices_file()) {
            *self.prices.list.lock().unwrap() = Some(Arc::new(list));
        }
    }

    /// Fetch the list when there is none or it is more than a day old.
    pub(crate) fn prices_refresh(&self) {
        if !self.cfg.prices.auto {
            return;
        }
        let now = now_ms();
        let fresh = self
            .prices
            .list
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|l| now.saturating_sub(l.fetched_ms) < DAY_MS);
        {
            let mut last = self.prices.last_try_ms.lock().unwrap();
            if fresh || (*last != 0 && now.saturating_sub(*last) < RETRY_MS) {
                return;
            }
            *last = now;
        }
        let d = self.clone();
        tokio::spawn(async move {
            let url = d.cfg.prices.url.as_deref().unwrap_or(DEFAULT_URL);
            match PriceList::refresh(url, &d.paths.prices_file()).await {
                Ok(list) => {
                    *d.prices.list.lock().unwrap() = Some(Arc::new(list));
                    d.emit("station.prices", None, None, serde_json::json!({}));
                }
                // Not the owner's problem: the old copy, if any, stays in use.
                Err(e) => tracing::warn!(error = %e, "price list not fetched"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Trimmed from the real catalogue: its shape, a few of its entries.
    fn catalogue() -> Value {
        serde_json::from_str(include_str!("../fixtures/prices/models-dev.json")).unwrap()
    }

    #[test]
    fn a_model_is_matched_exactly_then_normalised_and_never_under_another_provider() {
        let list = PriceList::parse(&catalogue(), "fixture", 5).unwrap();
        let exact = list.find(Some("anthropic"), "claude-opus-5-5").unwrap();
        assert_eq!(exact.input, Some(4.0));
        assert_eq!(exact.context, Some(1_000_000));
        // A date suffix, a [1m] suffix, a provider prefix, upper case.
        for id in [
            "claude-opus-5-5-20260901",
            "claude-opus-5-5@20260901",
            "claude-opus-5-5-2026-09-01",
            "claude-opus-5-5[1m]",
            "anthropic/claude-opus-5-5",
            "Claude-Opus-5-5",
        ] {
            assert_eq!(list.find(Some("anthropic"), id), Some(exact), "{id}");
        }
        // The reseller in the fixture lists the same model at another price.
        assert_eq!(
            list.find(Some("reseller"), "claude-opus-5-5")
                .unwrap()
                .input,
            Some(9.0)
        );
        assert_eq!(list.find(Some("openai"), "claude-opus-5-5"), None);
        assert_eq!(list.find(None, "claude-opus-5-5"), None);
        assert_eq!(list.find(Some("anthropic"), "claude-unheard-of-9"), None);
    }

    #[test]
    fn cost_is_tokens_times_the_list_price_and_unknown_when_a_needed_price_is_missing() {
        let list = PriceList::parse(&catalogue(), "fixture", 5).unwrap();
        let cfg = PricesConfig::default();
        let prices = Prices {
            list: Some(Arc::new(list)),
            cfg: &cfg,
        };
        let tally = Tally {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 100_000,
        };
        // 4 + 0.5*20 + 2*0.2 + 0.1*5
        let cost = prices
            .cost(Some("anthropic"), "claude-opus-5-5", &tally)
            .unwrap();
        assert!((cost - 14.9).abs() < 1e-9, "{cost}");
        // The free model has no cache_write price: fine until some are used.
        let no_writes = Tally {
            cache_write: 0,
            ..tally
        };
        assert_eq!(
            prices.cost(Some("opencode"), "deepseek-v4.1-flash-free", &no_writes),
            Some(0.0)
        );
        assert_eq!(
            prices.cost(Some("opencode"), "deepseek-v4.1-flash-free", &tally),
            None
        );
        // Listed with a window but no cost.
        assert_eq!(prices.cost(Some("openai"), "window-only", &tally), None);
        assert_eq!(
            prices.find(Some("openai"), "window-only").unwrap().context,
            Some(64_000)
        );
        // The owner's own figure replaces the list's, one field at a time.
        let mut cfg = PricesConfig::default();
        cfg.model.insert(
            "claude-opus-5-5".into(),
            Price {
                output: Some(0.0),
                ..Default::default()
            },
        );
        let own = Prices {
            list: prices.list.clone(),
            cfg: &cfg,
        };
        let cost = own
            .cost(Some("anthropic"), "claude-opus-5-5", &tally)
            .unwrap();
        assert!((cost - 4.9).abs() < 1e-9, "{cost}");
    }

    /// One request answered with `body`, then the port is closed.
    fn serve_once(status: &str, body: String) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api.json", listener.local_addr().unwrap());
        let status = status.to_string();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    #[tokio::test]
    async fn the_list_is_fetched_and_kept_and_a_failed_fetch_keeps_the_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prices.json");
        let url = serve_once("200 OK", catalogue().to_string());
        let list = PriceList::refresh(&url, &file).await.unwrap();
        assert_eq!(list.source, url);
        assert!(list.fetched_ms > 0);
        assert_eq!(PriceList::load(&file), Some(list.clone()));

        // A server error, a body that is not a price list, and nobody home.
        let broken = serve_once("500 Internal Server Error", "{}".into());
        assert!(PriceList::refresh(&broken, &file).await.is_err());
        let wrong = serve_once("200 OK", json!({"hello": "world"}).to_string());
        assert!(PriceList::refresh(&wrong, &file).await.is_err());
        assert!(PriceList::refresh(&url, &file).await.is_err());
        assert_eq!(PriceList::load(&file), Some(list));
    }
}
