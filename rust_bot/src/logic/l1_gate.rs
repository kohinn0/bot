use tokio::sync::{Mutex, Semaphore};

/// Egy HL-fiók L1 akciói (nonce + aláírás + POST) sorban fussanak, monoton nonce-pal.
///
/// **Design:**
/// - `Semaphore(1)`: egyszerre csak egy L1 művelet futhat — soros végrehajtás garantált.
/// - `Mutex<u64>` nonce-hoz: **mikroszekundumos** zárolás (csak a számláló növelése),
///   nem a teljes hálózati kérés ideje alatt tart — ez volt az eredeti hiba.
/// - Az összes többi task `sem.acquire()` mögé vár, de a nonce-mutex hamar felszabadul.
pub struct HyperliquidL1Gate {
    sem: Semaphore,
    last_nonce: Mutex<u64>,
}

impl HyperliquidL1Gate {
    pub fn new() -> Self {
        Self {
            sem: Semaphore::new(1),
            last_nonce: Mutex::new(Self::now_ms()),
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Aszinkron L1 művelet sorba helyezése és végrehajtása.
    ///
    /// Garantálja:
    /// 1. Egyszerre csak egy L1 hívás fut (Semaphore).
    /// 2. A nonce-mutex csak a számláló növelésig tart (< 1 ms), nem a hálózati válaszig.
    /// 3. A nonce szigorúan monoton növekvő minden hívás közt.
    pub async fn run<F, Fut, T>(&self, op: F) -> T
    where
        F: FnOnce(u64) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // Szemafor: soros végrehajtás — a következő task itt vár, amíg az előző végez
        let _permit = self.sem.acquire().await.expect("L1Gate semaphore zárt");

        // Nonce kiosztás: rövid zárolás, azonnal felszabadul
        let nonce = {
            let mut last = self.last_nonce.lock().await;
            let wall = Self::now_ms();
            let n = (*last).max(wall).saturating_add(1);
            *last = n;
            n
        }; // <- nonce-mutex felszabadul itt, a hálózati hívás ELŐTT

        // Hálózati hívás: nonce-mutex nélkül fut, de a szemafor még tart
        op(nonce).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn nonce_strictly_increases() {
        let g = Arc::new(HyperliquidL1Gate::new());
        let a = g.run(|n| async move { n }).await;
        let b = g.run(|n| async move { n }).await;
        assert!(b > a, "b={} a={}", b, a);
    }

    #[tokio::test]
    async fn concurrent_calls_serialized_and_unique() {
        let g = Arc::new(HyperliquidL1Gate::new());
        let g1 = g.clone();
        let g2 = g.clone();
        let (a, b) = tokio::join!(
            g1.run(|n| async move { n }),
            g2.run(|n| async move { n }),
        );
        assert_ne!(a, b, "nonce-ok nem egyezhetnek: a={} b={}", a, b);
    }
}
