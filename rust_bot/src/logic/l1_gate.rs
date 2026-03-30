use tokio::sync::Mutex;

/// Egy HL-fiók L1 akciói (nonce + aláírás + POST) sorban fussanak, monoton nonce-pal.
/// Két `tokio::spawn` (szignál vs reconcile) így nem keveri össze a sorrendet / nonce-ot.
pub struct HyperliquidL1Gate {
    last_nonce: Mutex<u64>,
}

impl HyperliquidL1Gate {
    pub fn new() -> Self {
        Self {
            last_nonce: Mutex::new(Self::now_ms()),
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// A zárolás a teljes `op(nonce).await` alatt tart — így párhuzamos task nem szúrhat közé.
    pub async fn run<F, Fut, T>(&self, op: F) -> T
    where
        F: FnOnce(u64) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let mut last = self.last_nonce.lock().await;
        let wall = Self::now_ms();
        let nonce = (*last).max(wall).saturating_add(1);
        *last = nonce;
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
}
