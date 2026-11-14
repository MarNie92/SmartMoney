//! Telegram message texts (German, HTML formatting).

use smartmoney_store::ScoredTrade;

/// Escape for Telegram HTML parse mode.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn cents(p: f64) -> String {
    format!("{:.1}¢", p * 100.0)
}

/// One simulated fill, as the alert needs it.
#[derive(Debug, Clone, Copy)]
pub struct Fill<'a> {
    pub strategy: &'a str,
    pub label: &'a str,
    pub shares: f64,
    pub avg_price: f64,
    pub cost_usd: f64,
    pub fee_usd: f64,
    pub slug: Option<&'a str>,
    pub event_slug: Option<&'a str>,
}

pub fn fill_message(f: &Fill<'_>) -> String {
    format!(
        "🧾 <b>Papier-Kauf ({})</b>\n{:.0} Anteile @ {} – Einsatz ${:.2} (Gebühr ${:.2})\n{}",
        escape(f.strategy),
        f.shares,
        cents(f.avg_price),
        f.cost_usd,
        f.fee_usd,
        linked(f.label, f.slug, f.event_slug)
    )
}

/// The text, as a link to the market when we know its slug.
fn linked(text: &str, slug: Option<&str>, event_slug: Option<&str>) -> String {
    let escaped = escape(text);
    match market_url(slug, event_slug) {
        Some(url) => format!("<a href=\"{url}\">{escaped}</a>"),
        None => escaped,
    }
}

/// One settled paper position, as the alert needs it.
#[derive(Debug, Clone, Copy)]
pub struct Settlement<'a> {
    pub strategy: &'a str,
    pub title: &'a str,
    pub shares: f64,
    pub payout_usd: f64,
    pub realized_pnl: f64,
    pub slug: Option<&'a str>,
    pub event_slug: Option<&'a str>,
}

pub fn settle_message(s: &Settlement<'_>) -> String {
    let icon = if s.realized_pnl >= 0.0 { "✅" } else { "❌" };
    format!(
        "{icon} <b>Abrechnung ({})</b> {:+.2}$\n{:.0} Anteile ausgezahlt: ${:.2}\n{}",
        escape(s.strategy),
        s.realized_pnl,
        s.shares,
        s.payout_usd,
        linked(s.title, s.slug, s.event_slug)
    )
}

/// Public Polymarket page of a market. The per-market slug is the most
/// precise; the event slug is the fallback. Combination markets
/// (parlays) carry neither, so they get no link.
pub fn market_url(slug: Option<&str>, event_slug: Option<&str>) -> Option<String> {
    // Only plain slug characters reach an href, so nothing can break out
    // of the attribute.
    fn clean(s: Option<&str>) -> Option<&str> {
        let s = s?.trim();
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(s)
    }
    if let Some(s) = clean(slug) {
        return Some(format!("https://polymarket.com/market/{s}"));
    }
    clean(event_slug).map(|e| format!("https://polymarket.com/event/{e}"))
}

pub fn wallet_url(address: &str) -> Option<String> {
    let ok = address.len() == 42
        && address.starts_with("0x")
        && address[2..].chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| format!("https://polymarket.com/profile/{address}"))
}

pub fn alert_message(s: &ScoredTrade) -> String {
    let title = escape(s.title.as_deref().unwrap_or("?"));
    let headline = match market_url(s.slug.as_deref(), s.event_slug.as_deref()) {
        Some(url) => format!("<a href=\"{url}\">{title}</a>"),
        None => title,
    };
    let outcome = s
        .outcome
        .as_deref()
        .map(|o| format!(" auf <b>{}</b>", escape(o)))
        .unwrap_or_default();
    let short = escape(&short_address(&s.proxy_wallet));
    let wallet = match wallet_url(&s.proxy_wallet) {
        Some(url) => format!("<a href=\"{url}\">Wallet {short}</a>"),
        None => format!("Wallet <code>{short}</code>"),
    };
    format!(
        "🚨 <b>Insider-Alert</b> Score {:.0}\n{headline}\n{} ${:.0}{outcome} @ {}\n{wallet}",
        s.total,
        escape(&s.side),
        s.usdc_value,
        cents(s.price),
    )
}

/// `0x1234…abcd` for display.
fn short_address(a: &str) -> String {
    if a.len() > 12 {
        format!("{}…{}", &a[..6], &a[a.len() - 4..])
    } else {
        a.to_owned()
    }
}

/// One strategy line of the daily digest / status reply.
pub struct DigestAccount {
    pub strategy: String,
    pub cash: f64,
    pub equity: Option<f64>,
    pub open: usize,
    pub settled_today: usize,
    pub realized_today: f64,
}

pub fn digest_message(date_label: &str, accounts: &[DigestAccount]) -> String {
    let mut out = format!("📊 <b>SmartMoney Papierhandel</b> ({})", escape(date_label));
    if accounts.is_empty() {
        out.push_str("\nKeine Strategie-Konten aktiv.");
    }
    for a in accounts {
        out.push_str(&format!(
            "\n<b>{}</b>: Kasse ${:.0}{}, offen {}, heute abgerechnet {}{}",
            escape(&a.strategy),
            a.cash,
            a.equity
                .map(|e| format!(", Wert ${e:.0}"))
                .unwrap_or_default(),
            a.open,
            a.settled_today,
            if a.settled_today > 0 {
                format!(" ({:+.2}$)", a.realized_today)
            } else {
                String::new()
            }
        ));
    }
    out
}

pub fn help_message(chat_id: i64) -> String {
    format!(
        "SmartMoney meldet hier Insider-Alerts und Papier-Trades.\n\
         Befehle: /status – Kontostand und offene Positionen.\n\
         Deine Chat-ID: <code>{chat_id}</code>"
    )
}

pub fn not_allowed_message(chat_id: i64) -> String {
    format!(
        "Diese Chat-ID ist nicht freigeschaltet: <code>{chat_id}</code>\n\
         Trage sie auf dem Rechner in SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS \
         (.env) ein und starte den Daemon neu."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        assert_eq!(escape("a<b>&c"), "a&lt;b&gt;&amp;c");
        let m = fill_message(&Fill {
            strategy: "favorites98",
            label: "Will <X> win?",
            shares: 103.2,
            avg_price: 0.962,
            cost_usd: 99.0,
            fee_usd: 0.2,
            slug: None,
            event_slug: None,
        });
        assert!(m.contains("Will &lt;X&gt; win?"));
        assert!(m.contains("103 Anteile @ 96.2¢"));
        assert!(m.contains("$99.00"));
    }

    fn scored(slug: Option<&str>, event_slug: Option<&str>, wallet: &str) -> ScoredTrade {
        ScoredTrade {
            score_id: 1,
            trade_id: 1,
            total: 72.0,
            wallet_freshness: 25.0,
            price_asymmetry: 25.0,
            relative_size: 20.0,
            concentration: 0.0,
            market_type: 15.0,
            cluster_bonus: 0.0,
            details_json: "{}".into(),
            scored_at: 0,
            proxy_wallet: wallet.into(),
            condition_id: "0xc".into(),
            token_id: "t".into(),
            side: "BUY".into(),
            outcome: Some("Yes".into()),
            outcome_index: Some(0),
            size: 100.0,
            price: 0.04,
            usdc_value: 4000.0,
            timestamp: 0,
            title: Some("Will X resign?".into()),
            slug: slug.map(str::to_owned),
            event_slug: event_slug.map(str::to_owned),
            category: Some("personnel".into()),
        }
    }

    #[test]
    fn links_point_at_polymarket() {
        assert_eq!(
            market_url(Some("gold-3000"), Some("what-will-gold-hit")).as_deref(),
            Some("https://polymarket.com/market/gold-3000")
        );
        // Empty per-market slug falls back to the event.
        assert_eq!(
            market_url(Some("  "), Some("what-will-gold-hit")).as_deref(),
            Some("https://polymarket.com/event/what-will-gold-hit")
        );
        assert_eq!(market_url(None, None), None);
        // Anything that is not a plain slug is refused, so no href can be
        // broken out of.
        assert_eq!(market_url(Some("a\" onload=x"), None), None);

        let w = "0x5be742be77feb9d9d888aa60987258d9daff989d";
        assert_eq!(
            wallet_url(w).as_deref(),
            Some("https://polymarket.com/profile/0x5be742be77feb9d9d888aa60987258d9daff989d")
        );
        assert_eq!(wallet_url("not-an-address"), None);
    }

    #[test]
    fn alert_links_market_and_wallet() {
        let w = "0x5be742be77feb9d9d888aa60987258d9daff989d";
        let m = alert_message(&scored(Some("will-x-resign"), None, w));
        assert!(
            m.contains(
                "<a href=\"https://polymarket.com/market/will-x-resign\">Will X resign?</a>"
            ),
            "{m}"
        );
        assert!(m.contains("https://polymarket.com/profile/0x5be7"), "{m}");
        assert!(m.contains("0x5be7…989d"), "{m}");
        assert!(m.contains("auf <b>Yes</b>"), "{m}");
        assert!(m.contains("4.0¢"), "{m}");

        // A parlay without slugs still produces a readable message.
        let plain = alert_message(&scored(None, None, "garbage"));
        assert!(!plain.contains("<a href"), "{plain}");
        assert!(plain.contains("Will X resign?"), "{plain}");
    }

    #[test]
    fn settle_and_digest_texts() {
        let s = settle_message(&Settlement {
            strategy: "anchor2",
            title: "T",
            shares: 100.0,
            payout_usd: 100.0,
            realized_pnl: -3.5,
            slug: Some("a-market"),
            event_slug: None,
        });
        assert!(s.starts_with("❌"));
        assert!(s.contains("-3.50$"));
        assert!(
            s.contains("<a href=\"https://polymarket.com/market/a-market\">T</a>"),
            "{s}"
        );
        let d = digest_message(
            "14.11.",
            &[DigestAccount {
                strategy: "favorites98".into(),
                cash: 4901.0,
                equity: Some(5004.0),
                open: 3,
                settled_today: 2,
                realized_today: 7.4,
            }],
        );
        assert!(d.contains("Kasse $4901"));
        assert!(d.contains("Wert $5004"));
        assert!(d.contains("heute abgerechnet 2 (+7.40$)"));
    }
}
