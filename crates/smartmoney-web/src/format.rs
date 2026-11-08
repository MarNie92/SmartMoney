//! Display helpers (German UI copy, USD amounts).

use serde_json::Value;
use smartmoney_core::MarketCategory;

/// `$5,400`, `$12.50` below $100.
pub fn usd(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let neg = v < 0.0;
    let abs = v.abs();
    let body = if abs < 100.0 {
        format!("{abs:.2}")
    } else {
        let whole = abs.round() as u64;
        let digits = whole.to_string();
        let mut out = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out
    };
    format!("{}${body}", if neg { "-" } else { "" })
}

/// Outcome price as cents: `0.608` -> `60.8¢`, `0.05` -> `5¢`.
pub fn cents(p: f64) -> String {
    let c = (p * 1000.0).round() / 10.0;
    if (c - c.round()).abs() < 1e-9 {
        format!("{c:.0}¢")
    } else {
        format!("{c:.1}¢")
    }
}

/// Compact relative time: `12 s`, `4 min`, `3 h`, `2 d`.
pub fn ago(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    match d {
        0..=59 => format!("{d} s"),
        60..=3599 => format!("{} min", d / 60),
        3600..=86_399 => format!("{} h", d / 3600),
        _ => format!("{} d", d / 86_400),
    }
}

/// `0x544aa000db6bef61…` -> `0x544a…041b`.
pub fn short_addr(a: &str) -> String {
    if a.len() <= 12 {
        return a.to_owned();
    }
    format!("{}…{}", &a[..6], &a[a.len() - 4..])
}

pub fn category_label(c: Option<&str>) -> &'static str {
    match c.and_then(|c| c.parse::<MarketCategory>().ok()) {
        Some(MarketCategory::Personnel) => "Personalie",
        Some(MarketCategory::Corporate) => "Firma",
        Some(MarketCategory::Legal) => "Gericht",
        Some(MarketCategory::Politics) => "Politik",
        Some(MarketCategory::Sports) => "Sport",
        Some(MarketCategory::CryptoPrice) => "Krypto-Kurs",
        Some(MarketCategory::Other) | None => "Sonstiges",
    }
}

pub fn days(d: f64) -> String {
    if d < 1.0 {
        format!("{:.0} h", (d * 24.0).max(0.0))
    } else if d < 10.0 {
        format!("{d:.1} Tage").replace('.', ",")
    } else {
        format!("{d:.0} Tage")
    }
}

fn pct(share: f64) -> String {
    let p = share * 100.0;
    if p < 1.0 {
        format!("{p:.1} %").replace('.', ",")
    } else {
        format!("{p:.0} %")
    }
}

/// Plain-language explanation of each score component from `details_json`.
pub struct Explanations {
    pub freshness: String,
    pub asymmetry: String,
    pub size: String,
    pub concentration: String,
    pub market_type: String,
    pub cluster: String,
}

pub fn explain(details: &Value, price: f64, category: Option<&str>) -> Explanations {
    if details.get("wallet_freshness").is_none() {
        let note = details
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("nicht bewertet");
        let msg = if note.contains("sell") {
            "Verkauf, wird nicht bewertet".to_owned()
        } else {
            note.to_owned()
        };
        return Explanations {
            freshness: msg.clone(),
            asymmetry: msg.clone(),
            size: msg.clone(),
            concentration: msg.clone(),
            market_type: msg.clone(),
            cluster: msg,
        };
    }
    let f = &details["wallet_freshness"]["inputs"];
    let freshness = match (f["age_days"].as_f64(), f["markets_traded"].as_i64()) {
        (Some(a), Some(m)) => format!("Wallet {} alt, {m} Märkte gehandelt", days(a)),
        (Some(a), None) => format!("Wallet {} alt", days(a)),
        (None, Some(m)) => format!("{m} Märkte gehandelt, Alter unbekannt"),
        (None, None) => {
            if f["note"]
                .as_str()
                .is_some_and(|n| n.contains("not enriched"))
            {
                "Wallet-Daten fehlen".to_owned()
            } else {
                "Keine Historie, wie ein neues Wallet behandelt".to_owned()
            }
        }
    };
    let s = &details["relative_size"];
    let size = match (s["share"].as_f64(), s["volume_24h_usd"].as_f64()) {
        (Some(share), Some(vol)) => format!("{} des 24h-Volumens ({})", pct(share), usd(vol)),
        _ => "24h-Volumen unbekannt".to_owned(),
    };
    let c = &details["concentration"];
    let concentration = match c["share"].as_f64() {
        Some(share) => format!("{} des Wallet-Kapitals in diesem Markt", pct(share)),
        None => "unbekannt".to_owned(),
    };
    let n = details["cluster_bonus"]["fresh_wallets"]
        .as_i64()
        .unwrap_or(0);
    let window = details["cluster_bonus"]["window_minutes"]
        .as_i64()
        .unwrap_or(30);
    let cluster = match n {
        0 => format!("Kein frisches Wallet in {window} min"),
        1 => format!("1 frisches Wallet in {window} min"),
        _ => format!("{n} frische Wallets in {window} min"),
    };
    Explanations {
        freshness,
        asymmetry: format!("Einstieg bei {}", cents(price)),
        size,
        concentration,
        market_type: format!("Kategorie {}", category_label(category)),
        cluster,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_money_and_prices() {
        assert_eq!(usd(5400.4), "$5,400");
        assert_eq!(usd(1_234_567.0), "$1,234,567");
        assert_eq!(usd(12.5), "$12.50");
        assert_eq!(usd(-250.0), "-$250");
        assert_eq!(cents(0.608), "60.8¢");
        assert_eq!(cents(0.05), "5¢");
        assert_eq!(cents(0.9999), "100¢");
    }

    #[test]
    fn formats_times_and_addresses() {
        assert_eq!(ago(100, 112), "12 s");
        assert_eq!(ago(0, 300), "5 min");
        assert_eq!(ago(0, 7200), "2 h");
        assert_eq!(ago(0, 200_000), "2 d");
        assert_eq!(ago(10, 5), "0 s");
        assert_eq!(
            short_addr("0x544aa000db6bef61d6163759f8475860b940041b"),
            "0x544a…041b"
        );
        assert_eq!(days(0.5), "12 h");
        assert_eq!(days(2.25), "2,2 Tage");
        assert_eq!(days(40.0), "40 Tage");
    }

    #[test]
    fn explains_components() {
        let d = json!({
            "wallet_freshness": {"factor": 1.0, "inputs": {"age_days": 2.0, "markets_traded": 3}},
            "relative_size": {"share": 0.27, "volume_24h_usd": 20000.0},
            "concentration": {"share": 1.0},
            "cluster_bonus": {"fresh_wallets": 2, "window_minutes": 30},
        });
        let e = explain(&d, 0.608, Some("sports"));
        assert_eq!(e.freshness, "Wallet 2,0 Tage alt, 3 Märkte gehandelt");
        assert_eq!(e.asymmetry, "Einstieg bei 60.8¢");
        assert_eq!(e.size, "27 % des 24h-Volumens ($20,000)");
        assert_eq!(e.concentration, "100 % des Wallet-Kapitals in diesem Markt");
        assert_eq!(e.market_type, "Kategorie Sport");
        assert_eq!(e.cluster, "2 frische Wallets in 30 min");
    }

    #[test]
    fn explains_sells_and_unknown_wallets() {
        let e = explain(&json!({"note": "sell trades are not scored"}), 0.5, None);
        assert_eq!(e.freshness, "Verkauf, wird nicht bewertet");
        let d = json!({
            "wallet_freshness": {"inputs": {"note": "wallet not enriched"}},
            "relative_size": {},
            "concentration": {},
            "cluster_bonus": {},
        });
        let e = explain(&d, 0.5, None);
        assert_eq!(e.freshness, "Wallet-Daten fehlen");
        assert_eq!(e.size, "24h-Volumen unbekannt");
        assert_eq!(e.market_type, "Kategorie Sonstiges");
    }
}
