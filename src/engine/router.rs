use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

pub const DEFAULT_EWMA_ALPHA: f64 = 0.25;

/// Telemetry metrics for a specific provider/instance endpoint.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InstanceMetrics {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(skip_serializing, default)]
    pub api_key: Option<String>,
    pub ewma_ttft_ms: f64,
    pub ewma_tps: f64,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub consecutive_errors: u32,
    pub last_status_code: Option<u16>,
    pub is_healthy: bool,
    pub custom_weight: f64,
}

impl InstanceMetrics {
    pub fn new(id: impl Into<String>, name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: base_url.into(),
            api_key: None,
            ewma_ttft_ms: 0.0,
            ewma_tps: 0.0,
            total_requests: 0,
            successful_requests: 0,
            consecutive_errors: 0,
            last_status_code: None,
            is_healthy: true,
            custom_weight: 1.0,
        }
    }

    #[allow(dead_code)]
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key;
        self
    }

    #[allow(dead_code)]
    pub fn with_custom_weight(mut self, weight: f64) -> Self {
        self.custom_weight = weight.max(0.01);
        self
    }

    /// Update EWMA telemetry on successful response.
    pub fn record_sample(&mut self, ttft_ms: f64, tps: f64) {
        self.total_requests += 1;
        self.successful_requests += 1;
        self.consecutive_errors = 0;
        self.last_status_code = Some(200);
        self.is_healthy = true;

        if self.successful_requests == 1 || self.ewma_tps <= 0.0 {
            self.ewma_ttft_ms = ttft_ms;
            self.ewma_tps = tps;
        } else {
            self.ewma_ttft_ms = (1.0 - DEFAULT_EWMA_ALPHA) * self.ewma_ttft_ms + DEFAULT_EWMA_ALPHA * ttft_ms;
            self.ewma_tps = (1.0 - DEFAULT_EWMA_ALPHA) * self.ewma_tps + DEFAULT_EWMA_ALPHA * tps;
        }
    }

    /// Record failure or non-200 HTTP status code.
    pub fn record_error(&mut self, status_code: Option<u16>) {
        self.total_requests += 1;
        self.consecutive_errors += 1;
        self.last_status_code = status_code;

        // Auto-mark unhealthy after 3 consecutive failures or severe server fault
        if self.consecutive_errors >= 3 || matches!(status_code, Some(500 | 502 | 503 | 504)) {
            self.is_healthy = false;
        }
    }

    /// Calculate routing score. Higher score = preferred instance.
    pub fn score(&self, _context_tokens: usize) -> f64 {
        if !self.is_healthy {
            return 0.0;
        }

        let effective_tps = self.ewma_tps.max(1.0);
        let ttft_sec = (self.ewma_ttft_ms / 1000.0).max(0.1);

        let reliability = if self.total_requests == 0 {
            1.0
        } else {
            self.successful_requests as f64 / self.total_requests as f64
        };

        let error_penalty = if self.consecutive_errors > 0 {
            0.5f64.powi(self.consecutive_errors as i32)
        } else {
            1.0
        };

        // Score formula: throughput weighted by TTFT, reliability and manual weight
        (effective_tps / ttft_sec) * reliability * error_penalty * self.custom_weight
    }
}

/// Registry of known provider endpoints with aggregated telemetry.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct InstanceRegistry {
    pub instances: HashMap<String, InstanceMetrics>,
}

impl InstanceRegistry {
    pub fn new() -> Self {
        Self {
            instances: HashMap::new(),
        }
    }

    pub fn register(&mut self, instance: InstanceMetrics) {
        self.instances.insert(instance.id.clone(), instance);
    }

    pub fn get(&self, id: &str) -> Option<&InstanceMetrics> {
        self.instances.get(id)
    }

    #[allow(dead_code)]
    pub fn get_mut(&mut self, id: &str) -> Option<&mut InstanceMetrics> {
        self.instances.get_mut(id)
    }

    #[allow(dead_code)]
    pub fn all(&self) -> Vec<&InstanceMetrics> {
        self.instances.values().collect()
    }

    /// Select the best-scoring healthy instance for the given context size.
    pub fn get_fastest(&self, context_tokens: usize) -> Option<&InstanceMetrics> {
        self.instances
            .values()
            .filter(|inst| inst.is_healthy)
            .max_by(|a, b| {
                let score_a = a.score(context_tokens);
                let score_b = b.score(context_tokens);
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    pub fn record_success(&mut self, id: &str, ttft_ms: f64, tps: f64) {
        if let Some(inst) = self.instances.get_mut(id) {
            inst.record_sample(ttft_ms, tps);
        }
    }

    pub fn record_error(&mut self, id: &str, status_code: Option<u16>) {
        if let Some(inst) = self.instances.get_mut(id) {
            inst.record_error(status_code);
        }
    }
}

/// Global thread-safe singleton instance registry.
pub static GLOBAL_INSTANCE_REGISTRY: LazyLock<Arc<RwLock<InstanceRegistry>>> =
    LazyLock::new(|| Arc::new(RwLock::new(InstanceRegistry::new())));

/// Session-level routing state for persistent instance affinity.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ConversationRouting {
    #[serde(default)]
    pub pinned_instance_id: Option<String>,
    #[serde(default)]
    pub last_ttft_ms: Option<u64>,
    #[serde(default)]
    pub last_tps: Option<f32>,
    #[serde(default)]
    pub consecutive_slow_turns: u32,
    #[serde(default)]
    pub manual_override: bool,
    #[serde(default)]
    pub total_routed_turns: u64,
}

impl Default for ConversationRouting {
    fn default() -> Self {
        Self {
            pinned_instance_id: None,
            last_ttft_ms: None,
            last_tps: None,
            consecutive_slow_turns: 0,
            manual_override: false,
            total_routed_turns: 0,
        }
    }
}

/// Reason why an affinity binding was evicted or re-routed.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum EvictionReason {
    ConsecutiveSlowTurns(u32),
    HardError(u16),
    UnhealthyInstance,
    ColdPrefillFasterThanSlowDecode {
        pinned_tps: f64,
        candidate_tps: f64,
    },
    ManualReset,
}

/// Routing policy decision emitted by PolicyEngine.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingAction {
    UsePinned {
        instance_id: String,
        reason: &'static str,
    },
    EvictAndSwitch {
        old_instance_id: String,
        new_instance_id: String,
        reason: EvictionReason,
    },
    InitialSelection {
        instance_id: String,
    },
    FallbackDefault {
        reason: &'static str,
    },
}

/// Policy engine configuring thresholds and evaluating sticky affinity vs. re-routing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoutingPolicy {
    pub min_acceptable_tps: f64,
    pub max_acceptable_ttft_ms: u64,
    pub max_consecutive_slow_turns: u32,
    pub small_context_token_cutoff: usize,
    pub switch_tps_multiplier: f64,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            min_acceptable_tps: 20.0,
            max_acceptable_ttft_ms: 7000,
            max_consecutive_slow_turns: 2,
            small_context_token_cutoff: 8192,
            switch_tps_multiplier: 1.8,
        }
    }
}

impl RoutingPolicy {
    /// Evaluates current session state against the instance registry and outputs the next routing action.
    pub fn decide_route(
        &self,
        routing: &ConversationRouting,
        registry: &InstanceRegistry,
        context_tokens: usize,
    ) -> RoutingAction {
        let best_candidate = registry.get_fastest(context_tokens);

        // Case 1: No pinned instance yet -> pick initial best
        let pinned_id = match &routing.pinned_instance_id {
            Some(id) => id,
            None => {
                return if let Some(best) = best_candidate {
                    RoutingAction::InitialSelection {
                        instance_id: best.id.clone(),
                    }
                } else {
                    RoutingAction::FallbackDefault {
                        reason: "Registry is empty or all instances are unhealthy",
                    }
                };
            }
        };

        // If manual override is active, respect user's explicit choice unless hard dead
        if routing.manual_override {
            return RoutingAction::UsePinned {
                instance_id: pinned_id.clone(),
                reason: "User manual override pinned",
            };
        }

        // Case 2: Pinned instance is missing or unhealthy in registry -> evict
        let pinned_inst = match registry.get(pinned_id) {
            Some(inst) if inst.is_healthy => inst,
            _ => {
                return if let Some(best) = best_candidate {
                    RoutingAction::EvictAndSwitch {
                        old_instance_id: pinned_id.clone(),
                        new_instance_id: best.id.clone(),
                        reason: EvictionReason::UnhealthyInstance,
                    }
                } else {
                    RoutingAction::FallbackDefault {
                        reason: "Pinned instance unhealthy and no alternative available",
                    }
                };
            }
        };

        // Case 3: Pinned instance suffered consecutive slow turns -> evict
        if routing.consecutive_slow_turns >= self.max_consecutive_slow_turns {
            if let Some(best) = best_candidate {
                if best.id != pinned_inst.id {
                    return RoutingAction::EvictAndSwitch {
                        old_instance_id: pinned_id.clone(),
                        new_instance_id: best.id.clone(),
                        reason: EvictionReason::ConsecutiveSlowTurns(routing.consecutive_slow_turns),
                    };
                }
            }
        }

        // Case 4: Tradeoff analysis: Cold Prefill vs Slow Generation
        // On small-to-medium contexts (< 8k tokens), cold prefill is fast (< 1s).
        // If an alternative instance offers significantly superior throughput (> 1.8x), evicting is faster overall!
        if context_tokens <= self.small_context_token_cutoff {
            if let Some(best) = best_candidate {
                if best.id != pinned_inst.id
                    && pinned_inst.ewma_tps > 0.0
                    && best.ewma_tps >= pinned_inst.ewma_tps * self.switch_tps_multiplier
                {
                    // Estimated time for 500 token generation:
                    // T_pinned = 500 / pinned_tps
                    // T_switch = cold_prefill_est (~0.8s) + 500 / best_tps
                    let t_pinned = 500.0 / pinned_inst.ewma_tps;
                    let t_switch = 0.8 + (500.0 / best.ewma_tps);

                    if t_switch < t_pinned {
                        return RoutingAction::EvictAndSwitch {
                            old_instance_id: pinned_id.clone(),
                            new_instance_id: best.id.clone(),
                            reason: EvictionReason::ColdPrefillFasterThanSlowDecode {
                                pinned_tps: pinned_inst.ewma_tps,
                                candidate_tps: best.ewma_tps,
                            },
                        };
                    }
                }
            }
        }

        // Default: Keep pinned instance to preserve KV cache
        RoutingAction::UsePinned {
            instance_id: pinned_id.clone(),
            reason: "Preserving KV prompt cache affinity",
        }
    }

    /// Process completion results of a turn: update metrics and degradation counters.
    pub fn update_after_turn(
        &self,
        routing: &mut ConversationRouting,
        registry: &mut InstanceRegistry,
        instance_id: &str,
        ttft_ms: f64,
        tps: f64,
        is_error: bool,
        status_code: Option<u16>,
    ) {
        routing.total_routed_turns += 1;

        if is_error {
            registry.record_error(instance_id, status_code);
            // Hard error codes immediately trigger degradation / eviction
            if matches!(status_code, Some(429 | 502 | 503 | 504)) {
                routing.consecutive_slow_turns = self.max_consecutive_slow_turns;
            }
            return;
        }

        registry.record_success(instance_id, ttft_ms, tps);
        routing.last_ttft_ms = Some(ttft_ms as u64);
        routing.last_tps = Some(tps as f32);

        // Check if this turn violated SLA
        let is_slow_tps = tps > 0.0 && tps < self.min_acceptable_tps;
        let is_slow_ttft = ttft_ms as u64 > self.max_acceptable_ttft_ms;

        if is_slow_tps || is_slow_ttft {
            routing.consecutive_slow_turns += 1;
        } else {
            routing.consecutive_slow_turns = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ewma_calculation() {
        let mut inst = InstanceMetrics::new("inst-1", "Provider A", "https://api.a.com");
        inst.record_sample(200.0, 50.0);
        assert_eq!(inst.ewma_ttft_ms, 200.0);
        assert_eq!(inst.ewma_tps, 50.0);

        // Second sample with alpha = 0.25:
        // ewma_tps = 0.75 * 50 + 0.25 * 90 = 37.5 + 22.5 = 60.0
        inst.record_sample(100.0, 90.0);
        assert!((inst.ewma_tps - 60.0).abs() < 1e-4);
        assert!((inst.ewma_ttft_ms - 175.0).abs() < 1e-4);
        assert_eq!(inst.successful_requests, 2);
        assert_eq!(inst.consecutive_errors, 0);
    }

    #[test]
    fn test_sticky_routing_happy_path() {
        let mut registry = InstanceRegistry::new();
        let mut inst1 = InstanceMetrics::new("fast-1", "Fast Node", "https://fast.com");
        inst1.record_sample(100.0, 80.0);
        registry.register(inst1);

        let policy = RoutingPolicy::default();
        let mut routing = ConversationRouting::default();

        // 1. Initial selection
        let action = policy.decide_route(&routing, &registry, 2000);
        assert_eq!(
            action,
            RoutingAction::InitialSelection {
                instance_id: "fast-1".to_string()
            }
        );

        // Pin to fast-1
        routing.pinned_instance_id = Some("fast-1".to_string());

        // 2. Subsequent turn should stay pinned for KV cache
        let action2 = policy.decide_route(&routing, &registry, 2500);
        assert_eq!(
            action2,
            RoutingAction::UsePinned {
                instance_id: "fast-1".to_string(),
                reason: "Preserving KV prompt cache affinity"
            }
        );
    }

    #[test]
    fn test_auto_eviction_on_consecutive_slow_turns() {
        let mut registry = InstanceRegistry::new();
        let mut inst_choked = InstanceMetrics::new("choked-1", "Choked Node", "https://choked.com");
        inst_choked.record_sample(300.0, 15.0); // Slow!
        registry.register(inst_choked);

        let mut inst_fast = InstanceMetrics::new("fast-2", "Fast Node", "https://fast.com");
        inst_fast.record_sample(120.0, 85.0);
        registry.register(inst_fast);

        let policy = RoutingPolicy::default();
        let routing = ConversationRouting {
            pinned_instance_id: Some("choked-1".to_string()),
            consecutive_slow_turns: 2, // Degraded!
            ..Default::default()
        };

        let action = policy.decide_route(&routing, &registry, 3000);
        assert_eq!(
            action,
            RoutingAction::EvictAndSwitch {
                old_instance_id: "choked-1".to_string(),
                new_instance_id: "fast-2".to_string(),
                reason: EvictionReason::ConsecutiveSlowTurns(2)
            }
        );
    }

    #[test]
    fn test_hard_error_degradation_trigger() {
        let mut registry = InstanceRegistry::new();
        let inst = InstanceMetrics::new("node-1", "Node", "https://node.com");
        registry.register(inst);

        let policy = RoutingPolicy::default();
        let mut routing = ConversationRouting {
            pinned_instance_id: Some("node-1".to_string()),
            ..Default::default()
        };

        policy.update_after_turn(
            &mut routing,
            &mut registry,
            "node-1",
            0.0,
            0.0,
            true,
            Some(429), // Too Many Requests
        );

        assert_eq!(routing.consecutive_slow_turns, policy.max_consecutive_slow_turns);
    }

    #[test]
    fn test_manual_override_respected() {
        let mut registry = InstanceRegistry::new();
        let mut inst_slow = InstanceMetrics::new("slow-node", "Slow", "https://slow.com");
        inst_slow.record_sample(400.0, 10.0);
        registry.register(inst_slow);

        let mut inst_fast = InstanceMetrics::new("fast-node", "Fast", "https://fast.com");
        inst_fast.record_sample(100.0, 95.0);
        registry.register(inst_fast);

        let policy = RoutingPolicy::default();
        let routing = ConversationRouting {
            pinned_instance_id: Some("slow-node".to_string()),
            manual_override: true,
            consecutive_slow_turns: 5,
            ..Default::default()
        };

        let action = policy.decide_route(&routing, &registry, 1000);
        assert_eq!(
            action,
            RoutingAction::UsePinned {
                instance_id: "slow-node".to_string(),
                reason: "User manual override pinned"
            }
        );
    }
}
