//! Локальное (и ТОЛЬКО локальное) оценивание guard-релеев клиентом.
//!
//! Клиент наблюдает СВОИ релеи — успех/таймаут запросов и задержку — и держится
//! за стабильные («липкость»), отбрасывая флапающие. Оценки никому не рассылаются:
//! публичный список guard'ов = список мишеней, а чужим оценкам верить нельзя
//! (геймится Sybil).
//!
//! ЧЕСТНО про границы: это оптимизирует НАДЁЖНОСТЬ/скорость, а не стойкость против
//! умного противника. Враждебный релей с идеальным аптаймом и низкой задержкой
//! наберёт высокий балл — поведение не отличить от «честный и быстрый». Защита от
//! такого = доверие через знакомых (F2F/vouching), отдельно. Здесь — только QoS и
//! отсев плохих.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bootstrap::NodeInfo;

/// Вес свежего наблюдения (EWMA): чем больше — тем быстрее реагируем на изменения.
const ALPHA: f64 = 0.3;
/// Сколько проб до полного доверия накопленной оценке (испытательный срок).
const PROBATION: f64 = 5.0;
/// Балл незнакомого релея — между «проверенно хорошим» и «проверенно плохим».
/// Незнакомцы пробуются, но проигрывают доказавшим себя guard'ам.
const EXPLORE_BASELINE: f64 = 0.4;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GuardStats {
    /// EWMA успеха запросов, 0..1.
    pub reliability: f64,
    /// EWMA задержки (мс), обновляется по успешным.
    pub latency_ms: f64,
    pub samples: u32,
    pub last_used: u64,
}

impl Default for GuardStats {
    fn default() -> Self {
        Self { reliability: 0.5, latency_ms: 500.0, samples: 0, last_used: 0 }
    }
}

/// Локальная база оценок релеев. Сериализуется для персистентности между запусками.
#[derive(Serialize, Deserialize, Default)]
pub struct GuardManager {
    stats: HashMap<String, GuardStats>,
}

impl GuardManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Восстановить из JSON (например, из localStorage / файла клиента).
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Записать исход использования релея (после каждого запроса через него).
    pub fn record(&mut self, relay_pubkey: &str, success: bool, latency_ms: u64) {
        let s = self.stats.entry(relay_pubkey.to_string()).or_default();
        s.reliability = ALPHA * (if success { 1.0 } else { 0.0 }) + (1.0 - ALPHA) * s.reliability;
        if success {
            s.latency_ms = ALPHA * latency_ms as f64 + (1.0 - ALPHA) * s.latency_ms;
        }
        s.samples = s.samples.saturating_add(1);
        s.last_used = now_unix();
    }

    /// Оценка релея (выше = лучше). Незнакомый → exploration-базлайн.
    /// Малосэмплированные плавно подтягиваются к базлайну (доверие растёт с числом проб).
    pub fn score(&self, relay_pubkey: &str) -> f64 {
        match self.stats.get(relay_pubkey) {
            None => EXPLORE_BASELINE,
            Some(s) => {
                let confidence = (s.samples as f64 / PROBATION).min(1.0);
                let latency_factor = 1.0 / (1.0 + s.latency_ms / 500.0);
                let proven = s.reliability * latency_factor;
                EXPLORE_BASELINE * (1.0 - confidence) + proven * confidence
            }
        }
    }

    /// Выбрать лучший guard среди релеев каталога. «Липкость» возникает сама:
    /// проверенный быстрый релей держится выше базлайна, пока не просядет надёжность.
    pub fn pick<'a>(&self, relays: &'a [NodeInfo]) -> Option<&'a NodeInfo> {
        relays
            .iter()
            .filter(|n| n.role == "relay")
            .max_by(|a, b| {
                self.score(&a.pubkey)
                    .partial_cmp(&self.score(&b.pubkey))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay(pk: &str) -> NodeInfo {
        NodeInfo { pubkey: pk.into(), address: "x".into(), role: "relay".into(), name: String::new() }
    }

    #[test]
    fn good_relay_outscores_bad() {
        let mut gm = GuardManager::new();
        for _ in 0..10 {
            gm.record("good", true, 50);
            gm.record("bad", false, 0);
        }
        assert!(gm.score("good") > gm.score("bad"));
        assert!(gm.score("good") > EXPLORE_BASELINE);
        assert!(gm.score("bad") < EXPLORE_BASELINE);
    }

    #[test]
    fn unknown_gets_exploration_baseline() {
        let gm = GuardManager::new();
        assert!((gm.score("never-seen") - EXPLORE_BASELINE).abs() < 1e-9);
    }

    #[test]
    fn picks_proven_guard_over_fresh() {
        let mut gm = GuardManager::new();
        for _ in 0..10 {
            gm.record("proven", true, 40);
        }
        let relays = vec![relay("proven"), relay("fresh1"), relay("fresh2")];
        assert_eq!(gm.pick(&relays).unwrap().pubkey, "proven");
    }

    #[test]
    fn degraded_guard_drops_below_baseline() {
        let mut gm = GuardManager::new();
        for _ in 0..10 {
            gm.record("g", true, 40);
        }
        assert!(gm.score("g") > EXPLORE_BASELINE);
        for _ in 0..20 {
            gm.record("g", false, 0);
        }
        assert!(gm.score("g") < EXPLORE_BASELINE); // уступит свежим/другим
    }

    #[test]
    fn persistence_roundtrip() {
        let mut gm = GuardManager::new();
        gm.record("a", true, 100);
        let restored = GuardManager::from_json(&gm.to_json());
        assert!((restored.score("a") - gm.score("a")).abs() < 1e-9);
    }
}
