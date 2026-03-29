use crate::config::StrategyConfig;
use crate::network::feed::L2BookState;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Időalapú bar (100ms aggregált return) ────────────────────────────────────
// A probléma az eredeti kódban: WS L2 tick ~1ms-enként jön, 200 tick = ~200ms.
// Ennyi tick-en a log_ret-ek szinte mind 0.0 → std_dev → 0 → z_score robban
// bármilyen apró elmozdulásra → folyamatos false signal.
//
// Megoldás: 100ms-es aggregált OHLC bar-ok. Minden bar = 100ms legjobb mid-je.
// 300 bar = 30 másodperc statisztikai ablak → stabil std_dev, megbízható z-score.
const BAR_DURATION_MS: u64 = 100;
const BAR_HISTORY: usize = 300; // 300 × 100ms = 30 másodperc
const WARMUP_BARS: usize = 30;  // legalább 3mp adat kell a z-score-hoz

pub struct SignalEngine {
    config: StrategyConfig,
    state_ref: Arc<RwLock<L2BookState>>,

    // Időalapú bar-ok (log_return-ök)
    bar_returns: VecDeque<f64>,

    // O(1) futó összesítők (numerikusan stabil)
    sum_x: f64,
    sum_x2: f64,
    recalc_counter: u64,

    // Aktuális bar állapota
    bar_open_mid: f64,
    bar_start_ms: u64,

    // Imbalance momentum
    prev_imbalance: f64,

    // Utolsó valós adat timestamp (staleness check)
    last_data_ts: u64,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig, state_ref: Arc<RwLock<L2BookState>>) -> Self {
        Self {
            config,
            state_ref,
            bar_returns: VecDeque::with_capacity(BAR_HISTORY + 1),
            sum_x: 0.0,
            sum_x2: 0.0,
            recalc_counter: 0,
            bar_open_mid: 0.0,
            bar_start_ms: 0,
            prev_imbalance: 0.5,
            last_data_ts: 0,
        }
    }

    pub async fn tick(&mut self) -> Option<SignalResult> {
        // 1. Adat lekérés – nem allokál
        let (mid_price, imbalance, ws_ts) = {
            let lock = self.state_ref.read().await;
            (lock.mid_price, lock.imbalance, lock.last_update_ts)
        };

        if mid_price <= 0.0 {
            return None;
        }

        // 2. Staleness check: ha a WS adat >500ms régi, hagyjuk ki
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        if ws_ts > 0 && now_ms.saturating_sub(ws_ts) > 500 {
            return None;
        }

        // 3. Bar inicializálás első tick-nél
        if self.bar_start_ms == 0 {
            self.bar_open_mid = mid_price;
            self.bar_start_ms = now_ms;
            return None;
        }

        // 4. Bar zárás időzítés: ha eltelt BAR_DURATION_MS
        let bar_age_ms = now_ms.saturating_sub(self.bar_start_ms);
        if bar_age_ms < BAR_DURATION_MS {
            // Még nem telt el a bar ideje – nem generálunk szignált
            return None;
        }

        // 5. Bar lezárása: log_return számítása (open → close = mid_price)
        let open = self.bar_open_mid;
        if open <= 0.0 || mid_price <= 0.0 {
            // Reset bar
            self.bar_open_mid = mid_price;
            self.bar_start_ms = now_ms;
            return None;
        }

        let log_ret = (mid_price / open).ln();

        // 6. Futó összesítők frissítése
        if self.bar_returns.len() >= BAR_HISTORY {
            if let Some(old) = self.bar_returns.pop_front() {
                self.sum_x -= old;
                self.sum_x2 -= old * old;
            }
        }
        self.bar_returns.push_back(log_ret);
        self.sum_x += log_ret;
        self.sum_x2 += log_ret * log_ret;

        // Lebegőpontos drift-újraszámítás 200 bar-onként
        self.recalc_counter += 1;
        if self.recalc_counter % 200 == 0 {
            self.sum_x = self.bar_returns.iter().sum();
            self.sum_x2 = self.bar_returns.iter().map(|&x| x * x).sum();
        }

        // Új bar nyitása
        self.bar_open_mid = mid_price;
        self.bar_start_ms = now_ms;

        // 7. Warmup: minimum WARMUP_BARS bar kell
        let n = self.bar_returns.len() as f64;
        if (n as usize) < WARMUP_BARS {
            return None;
        }

        // 8. Z-score számítás (numerikusan stabil)
        let mean = self.sum_x / n;
        let variance = ((self.sum_x2 / n) - (mean * mean)).max(0.0);
        let std_dev = variance.sqrt();

        // Minimális std_dev guard: ha a piac teljesen mozdulatlan,
        // ne generáljunk szignált (pl. éjjeli alacsony volumen).
        // SOL-on 0.001% alatti napi volatilitás nem reális kereskedési ablak.
        let min_std_dev = 0.00005; // ~0.005% per 100ms bar → évi ~25% vol
        if std_dev < min_std_dev {
            return None;
        }

        let z_score = (log_ret - mean) / std_dev;
        let volatility_px = (std_dev * mid_price).abs();

        // 9. Imbalance momentum
        let imb_momentum = imbalance - self.prev_imbalance;
        self.prev_imbalance = imbalance;

        // 10. Szignál logika
        // Küszöbök:
        //   z_threshold: config-ból (strategy_maker.json: 3.5 ajánlott)
        //   imbalance: 0.65/0.35 (közepes vételi/eladói nyomás)
        //   VAGY momentum: ±0.08 (gyors imbalance elmozdulás)
        //
        // Mean-reversion logika:
        //   z < -threshold → ár erősen esett → Buy (visszapattanás várható)
        //   z > +threshold → ár erősen nőtt → Sell (visszapattanás várható)
        //
        // Megerősítés: imbalance irány megegyezik a várható elmozdulással
        let threshold = self.config.z_score_threshold;

        if z_score < -threshold && (imbalance > 0.65 || imb_momentum > 0.08) {
            return Some(SignalResult {
                side: "Buy".to_string(),
                target_mid: mid_price,
                volatility: volatility_px,
                z_score,
                bar_count: self.bar_returns.len(),
            });
        }

        if z_score > threshold && (imbalance < 0.35 || imb_momentum < -0.08) {
            return Some(SignalResult {
                side: "Sell".to_string(),
                target_mid: mid_price,
                volatility: volatility_px,
                z_score,
                bar_count: self.bar_returns.len(),
            });
        }

        None
    }
}

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
    pub volatility: f64,
    /// Debug: z-score értéke a szignálnál (logoláshoz)
    pub z_score: f64,
    /// Debug: hány bar van a history-ban (warmup követés)
    pub bar_count: usize,
}
