//! Server-rendered pages (maud). UI copy is German.

use crate::format::{ago, category_label, cents, days, explain, short_addr, usd};
use maud::{html, Markup, DOCTYPE};
use smartmoney_core::config::ScoreWeights;
use smartmoney_store::sim::{SimAccount, SimPosition};
use smartmoney_store::{ScoredTrade, WalletRecord};

/// Collector polls older than this are flagged as stale.
const STALE_SECONDS: i64 = 180;

pub struct Status {
    pub now: i64,
    pub trades_24h: i64,
    pub alerts_24h: i64,
    pub threshold: f64,
    pub last_poll: Option<i64>,
}

pub struct PositionView {
    pub title: Option<String>,
    pub value: Option<f64>,
    pub cost: Option<f64>,
}

fn layout(title: &str, nav: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="de" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                meta name="robots" content="noindex";
                title { (title) }
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,400..800&display=swap";
                link rel="stylesheet" href="/static/app.css";
            }
            body {
                div.wrap {
                    header.top {
                        a.brand href="/" { "SmartMoney" }
                        nav {
                            a href="/" aria-current=[(nav == "feed").then_some("page")] { "Feed" }
                            a href="/sim" aria-current=[(nav == "sim").then_some("page")] { "Papierhandel" }
                        }
                    }
                    (body)
                    footer { "Nur Analyse und Papierhandel. SmartMoney platziert keine echten Orders." }
                }
                script src="/static/app.js" defer {}
            }
        }
    }
}

fn status_line(s: &Status) -> Markup {
    let stale = s.last_poll.is_none_or(|t| s.now - t > STALE_SECONDS);
    html! {
        p.status {
            span.pulse.stale[stale] aria-hidden="true" {}
            strong { (s.trades_24h) } " große Trades in 24 h, "
            strong { (s.alerts_24h) } " davon ab Schwelle " (format!("{:.0}", s.threshold)) ". "
            @match s.last_poll {
                Some(t) => { "Letzter Abruf vor " (ago(t, s.now)) "." }
                None => { "Noch kein Abruf – läuft der Daemon?" }
            }
        }
    }
}

struct Segment {
    class: &'static str,
    label: &'static str,
    points: f64,
    max: f64,
}

fn segments(s: &ScoredTrade, w: &ScoreWeights) -> [Segment; 6] {
    [
        Segment {
            class: "s-fresh",
            label: "Frische",
            points: s.wallet_freshness,
            max: w.wallet_freshness,
        },
        Segment {
            class: "s-asym",
            label: "Asymmetrie",
            points: s.price_asymmetry,
            max: w.price_asymmetry,
        },
        Segment {
            class: "s-size",
            label: "Größe",
            points: s.relative_size,
            max: w.relative_size,
        },
        Segment {
            class: "s-conc",
            label: "Konzentration",
            points: s.concentration,
            max: w.concentration,
        },
        Segment {
            class: "s-type",
            label: "Marktart",
            points: s.market_type,
            max: w.market_type,
        },
        Segment {
            class: "s-cluster",
            label: "Cluster",
            points: s.cluster_bonus,
            max: w.cluster_bonus,
        },
    ]
}

fn fill_pct(points: f64, max: f64) -> f64 {
    if max > 0.0 {
        (points / max * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn pts(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}").replace('.', ",")
    }
}

pub fn legend(w: &ScoreWeights) -> Markup {
    let items = [
        ("--c-fresh", "Frische", w.wallet_freshness),
        ("--c-asym", "Asymmetrie", w.price_asymmetry),
        ("--c-size", "Größe", w.relative_size),
        ("--c-conc", "Konzentration", w.concentration),
        ("--c-type", "Marktart", w.market_type),
        ("--c-cluster", "Cluster", w.cluster_bonus),
    ];
    html! {
        ul.legend aria-label="Score-Bestandteile, Breite entspricht dem Gewicht" {
            @for (var, label, max) in items {
                li style=(format!("--sw: var({var})")) { (label) " " (pts(max)) }
            }
        }
    }
}

pub fn row(s: &ScoredTrade, w: &ScoreWeights, threshold: f64, now: i64) -> Markup {
    let alert = s.total >= threshold;
    let mid = !alert && s.total >= threshold * 0.66;
    let segs = segments(s, w);
    let details: serde_json::Value = serde_json::from_str(&s.details_json).unwrap_or_default();
    let why = explain(&details, s.price, s.category.as_deref());
    let explanations = [
        &why.freshness,
        &why.asymmetry,
        &why.size,
        &why.concentration,
        &why.market_type,
        &why.cluster,
    ];
    let aria = segs
        .iter()
        .map(|g| format!("{} {} von {}", g.label, pts(g.points), pts(g.max)))
        .collect::<Vec<_>>()
        .join(", ");
    let is_buy = s.side == "BUY";
    html! {
        li.row.alert[alert] data-id=(s.score_id) {
            details {
                summary {
                    div.score.mid[mid] aria-label=(format!("Score {:.0}", s.total)) { (format!("{:.0}", s.total)) }
                    div {
                        p.q { (s.title.as_deref().unwrap_or("Unbekannter Markt")) }
                        p.trade {
                            span.side.sell[!is_buy] { @if is_buy { "Kauf" } @else { "Verkauf" } }
                            " " (usd(s.usdc_value)) " zu " (cents(s.price))
                            @if let Some(o) = &s.outcome { " " span.outcome { "auf „" (o) "“" } }
                        }
                        div.strip role="img" aria-label=(aria) {
                            @for g in &segs {
                                span class=(format!("seg {}", g.class)) style=(format!("flex-grow: {}", g.max.max(0.1))) {
                                    i style=(format!("width: {:.0}%", fill_pct(g.points, g.max))) {}
                                }
                            }
                        }
                        p.meta {
                            span { (category_label(s.category.as_deref())) }
                            span { "vor " (ago(s.timestamp, now)) }
                            span { "Wallet " (short_addr(&s.proxy_wallet)) }
                        }
                    }
                }
                dl.why {
                    @for (g, text) in segs.iter().zip(explanations) {
                        div class=(g.class) {
                            dt { (g.label) }
                            dd.pts { (pts(g.points)) " / " (pts(g.max)) }
                            dd { (text) }
                        }
                    }
                }
                p.links {
                    a href=(format!("/wallet/{}", s.proxy_wallet)) { "Wallet ansehen" }
                    @if let Some(slug) = s.event_slug.as_deref().or(s.slug.as_deref()) {
                        a href=(format!("https://polymarket.com/event/{slug}")) target="_blank" rel="noopener noreferrer" { "Markt auf Polymarket" }
                    }
                }
            }
        }
    }
}

pub fn live_region(status: &Status, rows: &[ScoredTrade], w: &ScoreWeights, min: f64) -> Markup {
    html! {
        (status_line(status))
        @if rows.is_empty() {
            p.empty {
                @if min > 0.0 {
                    "Noch keine Trades ab Score " (format!("{min:.0}")) ". Wähle „Alle“, um jeden großen Trade zu sehen."
                } @else {
                    "Noch keine bewerteten Trades. Sobald der Daemon große Trades findet, erscheinen sie hier."
                }
            }
        } @else {
            ul.feed {
                @for s in rows { (row(s, w, status.threshold, status.now)) }
            }
        }
    }
}

pub fn feed_page(status: &Status, rows: &[ScoredTrade], w: &ScoreWeights, min: f64) -> Markup {
    let t = status.threshold;
    let filters = [
        (0.0, "Alle".to_owned()),
        (40.0, "ab 40".to_owned()),
        (t, format!("Alerts (ab {t:.0})")),
    ];
    layout(
        "Feed – SmartMoney",
        "feed",
        html! {
            nav.filters aria-label="Mindest-Score" {
                @for (value, label) in &filters {
                    a href=(format!("/?min={value:.0}")) aria-current=[((min - value).abs() < 0.5).then_some("true")] { (label) }
                }
            }
            (legend(w))
            div #live data-src=(format!("/live?min={min:.0}")) aria-live="polite" {
                (live_region(status, rows, w, min))
            }
        },
    )
}

pub fn wallet_page(
    address: &str,
    wallet: Option<&WalletRecord>,
    positions: &[PositionView],
    scores: &[ScoredTrade],
    w: &ScoreWeights,
    threshold: f64,
    now: i64,
) -> Markup {
    let age = wallet
        .and_then(WalletRecord::created_at)
        .map(|c| days((now - c) as f64 / 86_400.0));
    html! {
        (layout(&format!("Wallet {}", short_addr(address)), "", html! {
            h1 { "Wallet " (short_addr(address)) }
            p.addr { (address) }
            @if let Some(name) = wallet.and_then(|w| w.name.as_deref().or(w.pseudonym.as_deref())) {
                p { "Profilname: " (name) }
            }
            dl.facts {
                div { dt { "Alter" } dd { (age.unwrap_or_else(|| "unbekannt".into())) } }
                div { dt { "Märkte gehandelt" } dd { (wallet.and_then(|w| w.markets_traded).map_or("–".into(), |m| m.to_string())) } }
                div { dt { "Trades gesamt" } dd { (wallet.and_then(|w| w.trade_count).map_or("–".into(), |m| m.to_string())) } }
                div { dt { "Offene Positionen" } dd { (wallet.and_then(|w| w.total_positions_usd).map_or("–".into(), usd)) } }
            }
            p {
                a href=(format!("https://polymarket.com/profile/{address}")) target="_blank" rel="noopener noreferrer" { "Profil auf Polymarket" }
            }
            @if wallet.is_none() {
                p.empty { "Zu diesem Wallet liegen noch keine Daten vor." }
            }
            h2 { "Bewertete Trades" }
            @if scores.is_empty() {
                p.empty { "Keine bewerteten Trades." }
            } @else {
                ul.feed { @for s in scores { (row(s, w, threshold, now)) } }
            }
            h2 { "Offene Positionen" }
            @if positions.is_empty() {
                p.empty { "Keine offenen Positionen bekannt." }
            } @else {
                table.pos {
                    thead { tr { th { "Markt" } th.num { "Wert" } th.num { "Einsatz" } } }
                    tbody {
                        @for p in positions {
                            tr {
                                td { (p.title.as_deref().unwrap_or("Markt nicht im Cache")) }
                                td.num { (p.value.map_or("–".into(), usd)) }
                                td.num { (p.cost.map_or("–".into(), usd)) }
                            }
                        }
                    }
                }
            }
        }))
    }
}

pub fn login_page() -> Markup {
    layout(
        "Anmelden – SmartMoney",
        "",
        html! {
            form.login method="get" action="/" {
                h1 { "Token eingeben" }
                p.status { "Das Dashboard ist geschützt. Den Token findest du in SMARTMONEY_DASHBOARD_TOKEN auf dem Server." }
                label for="token" { "Dashboard-Token" }
                input #token type="password" name="token" autocomplete="current-password" required;
                button type="submit" { "Anmelden" }
            }
        },
    )
}

pub fn not_found() -> Markup {
    layout(
        "Nicht gefunden – SmartMoney",
        "",
        html! { p.empty { "Diese Seite gibt es nicht. " a href="/" { "Zum Feed" } } },
    )
}

/// One paper-trading account with its latest positions and equity.
pub struct SimAccountView {
    pub account: SimAccount,
    pub positions: Vec<SimPosition>,
    pub equity: Option<f64>,
}

fn sim_summary(v: &SimAccountView) -> Markup {
    let settled: Vec<&SimPosition> = v
        .positions
        .iter()
        .filter(|p| p.status == "settled")
        .collect();
    let open = v.positions.iter().filter(|p| p.status == "open").count();
    let realized: f64 = settled.iter().filter_map(|p| p.realized_pnl).sum();
    let wins = settled
        .iter()
        .filter(|p| p.realized_pnl.unwrap_or(0.0) > 0.0)
        .count();
    html! {
        p.status {
            "Kasse " (usd(v.account.cash))
            @if let Some(e) = v.equity { " · Gesamtwert " (usd(e)) }
            " · offen " (open)
            " · abgerechnet " (settled.len())
            @if !settled.is_empty() {
                " (" (wins) " Gewinne) · realisiert "
                span.pnl.neg[realized < 0.0] { (format!("{realized:+.2}$")) }
            }
        }
    }
}

pub fn sim_page(accounts: &[SimAccountView], now: i64) -> Markup {
    layout(
        "Papierhandel – SmartMoney",
        "sim",
        html! {
            h1 { "Papierhandel" }
            @if accounts.is_empty() {
                p.empty { "Noch keine Strategie-Konten. Der Daemon legt sie beim Start an." }
            }
            @for v in accounts {
                section.sim-account {
                    h2 { (v.account.strategy) }
                    (sim_summary(v))
                    @if v.positions.is_empty() {
                        p.empty { "Noch keine Positionen." }
                    } @else {
                        table.sim {
                            thead {
                                tr {
                                    th { "Zeit" }
                                    th { "Markt" }
                                    th.num { "Anteile" }
                                    th.num { "Kurs" }
                                    th.num { "Einsatz" }
                                    th.num { "Ergebnis" }
                                }
                            }
                            tbody {
                                @for p in &v.positions {
                                    tr {
                                        td { (ago(p.opened_at, now)) }
                                        td.title {
                                            (p.title.as_deref().unwrap_or(&p.condition_id))
                                        }
                                        td.num { (format!("{:.0}", p.shares)) }
                                        td.num { (cents(p.avg_price)) }
                                        td.num { (usd(p.cost_usd)) }
                                        td.num {
                                            @match p.realized_pnl {
                                                Some(r) => span.pnl.neg[r < 0.0] { (format!("{r:+.2}$")) },
                                                None => span.muted { "offen" },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}
