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

pub fn fill_message(
    strategy: &str,
    label: &str,
    shares: f64,
    avg_price: f64,
    cost_usd: f64,
    fee_usd: f64,
) -> String {
    format!(
        "🧾 <b>Papier-Kauf ({})</b>\n{:.0} Anteile @ {} – Einsatz ${:.2} (Gebühr ${:.2})\n{}",
        escape(strategy),
        shares,
        cents(avg_price),
        cost_usd,
        fee_usd,
        escape(label)
    )
}

pub fn settle_message(
    strategy: &str,
    title: &str,
    shares: f64,
    payout_usd: f64,
    realized_pnl: f64,
) -> String {
    let icon = if realized_pnl >= 0.0 { "✅" } else { "❌" };
    format!(
        "{icon} <b>Abrechnung ({})</b> {:+.2}$\n{:.0} Anteile ausgezahlt: ${:.2}\n{}",
        escape(strategy),
        realized_pnl,
        shares,
        payout_usd,
        escape(title)
    )
}

pub fn alert_message(s: &ScoredTrade) -> String {
    format!(
        "🚨 <b>Insider-Alert</b> Score {:.0}\n{} ${:.0} @ {} – {}\nWallet <code>{}</code>",
        s.total,
        escape(&s.side),
        s.usdc_value,
        cents(s.price),
        escape(s.title.as_deref().unwrap_or("?")),
        escape(&s.proxy_wallet)
    )
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
        let m = fill_message("favorites98", "Will <X> win?", 103.2, 0.962, 99.0, 0.2);
        assert!(m.contains("Will &lt;X&gt; win?"));
        assert!(m.contains("103 Anteile @ 96.2¢"));
        assert!(m.contains("$99.00"));
    }

    #[test]
    fn settle_and_digest_texts() {
        let s = settle_message("anchor2", "T", 100.0, 100.0, -3.5);
        assert!(s.starts_with("❌"));
        assert!(s.contains("-3.50$"));
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
