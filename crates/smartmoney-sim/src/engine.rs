//! Fill planning against a best-first order book and equity math.

/// A planned taker buy: walk the asks until the stake is spent.
#[derive(Debug, Clone, PartialEq)]
pub struct FillPlan {
    pub shares: f64,
    /// Volume-weighted price without fees.
    pub avg_price: f64,
    /// Shares x price.
    pub notional_usd: f64,
    pub fee_usd: f64,
    /// Cash spent including fees.
    pub cost_usd: f64,
}

impl FillPlan {
    /// Share of the stake that the book could absorb.
    pub fn filled_fraction(&self, stake_usd: f64) -> f64 {
        if stake_usd <= 0.0 {
            return 0.0;
        }
        self.cost_usd / stake_usd
    }
}

/// Plan a taker buy of up to `stake_usd` (fees included) against asks in
/// best-first order. Fee per share: `rate x p x (1 - p)` at the level
/// price. Returns `None` when the book yields no shares.
pub fn plan_taker_buy(
    asks_best_first: &[(f64, f64)],
    stake_usd: f64,
    fee_rate: f64,
) -> Option<FillPlan> {
    let mut remaining = stake_usd;
    let (mut shares, mut notional, mut fee) = (0.0f64, 0.0f64, 0.0f64);
    for (price, size) in asks_best_first {
        let (p, sz) = (*price, *size);
        if !(0.0..=1.0).contains(&p) || sz <= 0.0 {
            continue;
        }
        let fee_per_share = fee_rate * p * (1.0 - p);
        let cost_per_share = p + fee_per_share;
        if cost_per_share <= 0.0 {
            continue;
        }
        let take = sz.min(remaining / cost_per_share);
        if take <= 1e-9 {
            break;
        }
        shares += take;
        notional += take * p;
        fee += take * fee_per_share;
        remaining -= take * cost_per_share;
        if remaining <= 0.01 {
            break;
        }
    }
    (shares > 1e-9).then(|| FillPlan {
        shares,
        avg_price: notional / shares,
        notional_usd: notional,
        fee_usd: fee,
        cost_usd: notional + fee,
    })
}

/// Account equity: cash plus marked open positions.
pub fn mark_equity(cash: f64, positions: &[(f64, f64)]) -> f64 {
    cash + positions
        .iter()
        .map(|(shares, mark)| shares * mark.clamp(0.0, 1.0))
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_a_single_deep_level() {
        // $100 at 96¢, 5 % fee: cost/share = 0.96 + 0.05*0.96*0.04 = 0.96192.
        let plan = plan_taker_buy(&[(0.96, 10_000.0)], 100.0, 0.05).unwrap();
        assert!((plan.shares - 100.0 / 0.96192).abs() < 1e-6);
        assert!((plan.avg_price - 0.96).abs() < 1e-12);
        assert!((plan.cost_usd - 100.0).abs() < 1e-6);
        assert!((plan.filled_fraction(100.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn walks_levels_and_stops_on_thin_book() {
        // 50 shares at 50¢, then 50 at 60¢; no fee for easy math.
        let asks = [(0.5, 50.0), (0.6, 50.0)];
        let plan = plan_taker_buy(&asks, 100.0, 0.0).unwrap();
        // $25 at 50¢ leaves $75 -> all 50 shares (cost 25), then 75/0.6 = 125
        // capped at 50 shares (cost 30): total 100 shares, $55 spent.
        assert!((plan.shares - 100.0).abs() < 1e-9);
        assert!((plan.cost_usd - 55.0).abs() < 1e-9);
        assert!((plan.avg_price - 0.55).abs() < 1e-9);
        assert!(plan.filled_fraction(100.0) < 0.6, "book too thin");

        assert_eq!(plan_taker_buy(&[], 100.0, 0.05), None);
        assert_eq!(plan_taker_buy(&[(1.5, 10.0)], 100.0, 0.05), None);
    }

    #[test]
    fn equity_marks_clamp() {
        let e = mark_equity(100.0, &[(50.0, 0.5), (10.0, 1.7)]);
        assert!((e - 135.0).abs() < 1e-9);
    }
}
