use crate::compositor::BuiltinValues;
use crate::config::{IpcMessage, OverlayConfig, ToastLevel};
use chrono::{DateTime, Local};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const MAX_METRIC_HISTORY: usize = 64;
const MAX_TOAST_STACK: usize = 5;

#[derive(Debug, Clone)]
struct SlotEntry {
    value: String,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct MetricSeries {
    pub history: VecDeque<u8>,
    pub latest: u8,
    pub label: Option<String>,
    pub expires_at: Option<Instant>,
    pub from_ipc: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ToastEntry {
    pub message: String,
    pub level: ToastLevel,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
struct DynamicOverlayEntry {
    config: OverlayConfig,
    expires_at: Option<Instant>,
}

/// Environment changes observed by a tick that can make event-driven commands due.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TickChanges {
    pub cwd_changed: bool,
    pub cmd_finished: bool,
}

/// In-memory state store inside `Compositor` holding TTL slots, metric series,
/// toast notification queues, dynamic overlays, and foreground command timers.
pub(crate) struct TelemetryStore {
    wall_clock: DateTime<Local>,
    builtins: BuiltinValues,
    active_fg_cmd: Option<(String, Instant)>,
    ipc_slots: BTreeMap<String, SlotEntry>,
    metrics: BTreeMap<String, MetricSeries>,
    toasts: VecDeque<ToastEntry>,
    dynamic_overlays: BTreeMap<String, DynamicOverlayEntry>,
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self {
            wall_clock: Local::now(),
            builtins: BuiltinValues::default(),
            active_fg_cmd: None,
            ipc_slots: BTreeMap::new(),
            metrics: BTreeMap::new(),
            toasts: VecDeque::new(),
            dynamic_overlays: BTreeMap::new(),
        }
    }

    pub fn on_tick(
        &mut self,
        wall_clock: DateTime<Local>,
        builtins: BuiltinValues,
        now: Instant,
    ) -> TickChanges {
        let changes = TickChanges {
            cwd_changed: self.builtins.cwd != builtins.cwd && builtins.cwd.is_some(),
            cmd_finished: self.builtins.fg_process.is_some() && builtins.fg_process.is_none(),
        };

        self.wall_clock = wall_clock;
        self.update_fg_telemetry(&builtins.fg_process, now);
        if let Some(cpu) = builtins.cpu_usage {
            self.push_metric_sample("cpu".to_string(), cpu, None, None, false);
        }
        if let Some(batt) = builtins.battery {
            self.push_metric_sample("battery".to_string(), batt, None, None, false);
        }
        self.builtins = builtins;
        self.evict_expired(now);
        changes
    }

    /// Records a sample produced by a metric command.
    pub fn push_metric(&mut self, key: String, value: u8) {
        self.push_metric_sample(key, value, None, None, false);
    }

    fn push_metric_sample(
        &mut self,
        key: String,
        value: u8,
        label: Option<String>,
        expires_at: Option<Instant>,
        from_ipc: bool,
    ) {
        let clamped = value.min(100);
        let entry = self.metrics.entry(key).or_insert_with(|| MetricSeries {
            history: VecDeque::with_capacity(MAX_METRIC_HISTORY),
            latest: clamped,
            label: None,
            expires_at: None,
            from_ipc,
        });
        if entry.history.len() >= MAX_METRIC_HISTORY {
            entry.history.pop_front();
        }
        entry.history.push_back(clamped);
        entry.latest = clamped;
        if label.is_some() {
            entry.label = label;
        }
        if expires_at.is_some() || from_ipc {
            entry.expires_at = expires_at;
        }
        if from_ipc {
            entry.from_ipc = true;
        }
    }

    pub fn apply_ipc(&mut self, msg: IpcMessage, now: Instant) {
        self.evict_expired(now);
        match msg {
            IpcMessage::SetSlot { key, value, ttl_ms } => {
                let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.ipc_slots.insert(key, SlotEntry { value, expires_at });
            }
            IpcMessage::ClearSlot { key } => {
                self.ipc_slots.remove(&key);
                self.metrics.remove(&key);
            }
            IpcMessage::PushMetric {
                key,
                value,
                label,
                ttl_ms,
            } => {
                let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.push_metric_sample(key, value, label, expires_at, true);
            }
            IpcMessage::Toast {
                message,
                level,
                ttl_ms,
            } => {
                if self.toasts.len() >= MAX_TOAST_STACK {
                    self.toasts.pop_front();
                }
                self.toasts.push_back(ToastEntry {
                    message,
                    level,
                    expires_at: now + Duration::from_millis(ttl_ms),
                });
            }
            IpcMessage::UpsertOverlay { overlay } => {
                let expires_at = overlay.ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.dynamic_overlays.insert(
                    overlay.id.clone(),
                    DynamicOverlayEntry {
                        config: overlay,
                        expires_at,
                    },
                );
            }
            IpcMessage::RemoveOverlay { id } => {
                self.dynamic_overlays.remove(&id);
            }
            IpcMessage::ReloadConfig | IpcMessage::Quit => {}
        }
    }

    pub fn wall_clock(&self) -> DateTime<Local> {
        self.wall_clock
    }

    pub fn builtins(&self) -> &BuiltinValues {
        &self.builtins
    }

    /// Name and start time of the current foreground command, if any.
    pub fn active_command(&self) -> Option<(&str, Instant)> {
        self.active_fg_cmd
            .as_ref()
            .map(|(name, started)| (name.as_str(), *started))
    }

    pub fn slot(&self, key: &str) -> Option<&str> {
        self.ipc_slots.get(key).map(|s| s.value.as_str())
    }

    pub fn metric(&self, key: &str) -> Option<&MetricSeries> {
        self.metrics.get(key)
    }

    pub fn metrics(&self) -> impl Iterator<Item = (&str, &MetricSeries)> {
        self.metrics.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Active toasts, oldest first.
    pub fn toasts(&self) -> impl DoubleEndedIterator<Item = &ToastEntry> {
        self.toasts.iter()
    }

    pub fn dynamic_overlays(&self) -> impl Iterator<Item = &OverlayConfig> {
        self.dynamic_overlays.values().map(|ov| &ov.config)
    }

    fn update_fg_telemetry(&mut self, current_fg: &Option<String>, now: Instant) {
        match current_fg {
            Some(proc_name) if !proc_name.is_empty() => match &self.active_fg_cmd {
                Some((existing, _)) if existing == proc_name => {}
                _ => {
                    self.active_fg_cmd = Some((proc_name.clone(), now));
                }
            },
            _ => {
                self.active_fg_cmd = None;
            }
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        self.ipc_slots
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.metrics
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.toasts.retain(|t| t.expires_at > now);
        self.dynamic_overlays
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
    }
}
