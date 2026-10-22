use crate::config::{CommandTrigger, WidgetKind, WidgetSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const DEFAULT_COMMAND_INTERVAL: Duration = Duration::from_millis(5000);
const DEFAULT_METRIC_INTERVAL: Duration = Duration::from_millis(2000);

/// Identifies a shell command scheduled by the HUD.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommandKey {
    /// A `command` widget, keyed by its widget `id`.
    Widget(String),
    /// A `sparkline` / `gauge` widget's `cmd`, keyed by its metric `key`.
    Metric(String),
}

/// A shell command whose schedule is due for asynchronous execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueCommand {
    pub key: CommandKey,
    pub cmd: String,
}

/// What changed in the environment since the previous scheduling pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScheduleContext<'a> {
    pub now: Instant,
    pub cwd_changed: bool,
    pub cmd_finished: bool,
    /// `when_files` marker names found in the current CWD or its ancestors.
    pub found_markers: &'a BTreeSet<String>,
}

/// A metric sample produced by a finished metric command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetricSample {
    pub key: String,
    pub value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandSpec {
    Widget {
        cmd: String,
        trigger: CommandTrigger,
        interval: Duration,
        when_files: Vec<String>,
    },
    Metric {
        cmd: String,
        interval: Duration,
    },
}

impl CommandSpec {
    fn of(kind: &WidgetKind) -> Option<(CommandKey, Self)> {
        match kind {
            WidgetKind::Command {
                id,
                cmd,
                trigger,
                interval_ms,
                when_files,
            } => {
                let trigger = trigger.unwrap_or(if interval_ms.is_some() {
                    CommandTrigger::Interval
                } else {
                    CommandTrigger::Change
                });
                let interval = interval_ms
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_COMMAND_INTERVAL);
                Some((
                    CommandKey::Widget(id.clone()),
                    CommandSpec::Widget {
                        cmd: cmd.clone(),
                        trigger,
                        interval,
                        when_files: when_files.clone(),
                    },
                ))
            }
            WidgetKind::Sparkline {
                key,
                cmd: Some(cmd),
                interval_ms,
                ..
            }
            | WidgetKind::Gauge {
                key,
                cmd: Some(cmd),
                interval_ms,
                ..
            } => Some((
                CommandKey::Metric(key.clone()),
                CommandSpec::Metric {
                    cmd: cmd.clone(),
                    interval: interval_ms
                        .map(Duration::from_millis)
                        .unwrap_or(DEFAULT_METRIC_INTERVAL),
                },
            )),
            _ => None,
        }
    }

    fn cmd(&self) -> &str {
        match self {
            CommandSpec::Widget { cmd, .. } | CommandSpec::Metric { cmd, .. } => cmd,
        }
    }
}

/// Decides when each widget or metric shell command is due, suppresses overlapping runs,
/// and interprets and keeps command output.
#[derive(Debug, Default)]
pub(crate) struct CommandScheduler {
    specs: BTreeMap<CommandKey, CommandSpec>,
    last_run: BTreeMap<CommandKey, Instant>,
    in_flight: BTreeSet<CommandKey>,
    outputs: BTreeMap<String, String>,
}

impl CommandScheduler {
    /// Returns the commands among `widgets` that are due now. A command whose spec changed
    /// since the previous pass (e.g. after a config reload) is due immediately.
    pub fn collect_due<'w>(
        &mut self,
        widgets: impl IntoIterator<Item = &'w WidgetSpec>,
        ctx: ScheduleContext<'_>,
    ) -> Vec<DueCommand> {
        let now = ctx.now;
        let mut seen = BTreeSet::new();
        let mut due = Vec::new();

        for widget in widgets {
            let Some((key, spec)) = CommandSpec::of(&widget.kind) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }

            if let CommandSpec::Widget { when_files, .. } = &spec
                && !markers_match(when_files, ctx.found_markers)
            {
                if let CommandKey::Widget(id) = &key {
                    self.outputs.remove(id);
                }
                self.last_run.remove(&key);
                self.specs.insert(key, spec);
                continue;
            }

            if self.specs.get(&key) != Some(&spec) {
                self.last_run.remove(&key);
            }

            let never_ran = !self.last_run.contains_key(&key);
            let is_due = match &spec {
                CommandSpec::Widget {
                    trigger: CommandTrigger::Interval,
                    interval,
                    ..
                }
                | CommandSpec::Metric { interval, .. } => self
                    .last_run
                    .get(&key)
                    .is_none_or(|&last| now.saturating_duration_since(last) >= *interval),
                CommandSpec::Widget {
                    trigger: CommandTrigger::Cwd,
                    ..
                } => never_ran || ctx.cwd_changed,
                CommandSpec::Widget {
                    trigger: CommandTrigger::Change,
                    ..
                } => never_ran || ctx.cwd_changed || ctx.cmd_finished,
            };

            if is_due && !self.in_flight.contains(&key) {
                self.last_run.insert(key.clone(), now);
                self.in_flight.insert(key.clone());
                due.push(DueCommand {
                    key: key.clone(),
                    cmd: spec.cmd().to_string(),
                });
            }
            self.specs.insert(key, spec);
        }
        due
    }

    /// Records that a dispatched command finished. `output` is `None` when it failed to run,
    /// in which case the previous output is kept. Returns the metric sample a metric command
    /// produced, if any.
    pub fn finish(&mut self, key: CommandKey, output: Option<&str>) -> Option<MetricSample> {
        self.in_flight.remove(&key);
        let first_line = output?.lines().next().unwrap_or("").trim();
        match key {
            CommandKey::Widget(id) => {
                self.outputs.insert(id, first_line.to_string());
                None
            }
            CommandKey::Metric(key) => {
                let value = first_line
                    .trim_end_matches('%')
                    .trim()
                    .parse::<f64>()
                    .ok()?;
                Some(MetricSample {
                    key,
                    value: value.round().clamp(0.0, 100.0) as u8,
                })
            }
        }
    }

    /// Latest output of the `command` widget `id`.
    pub fn output(&self, id: &str) -> Option<&str> {
        self.outputs.get(id).map(String::as_str)
    }
}

/// Union of the `when_files` marker names referenced by `widgets`.
pub(crate) fn watched_markers<'w>(
    widgets: impl IntoIterator<Item = &'w WidgetSpec>,
) -> BTreeSet<String> {
    widgets
        .into_iter()
        .filter_map(|w| match &w.kind {
            WidgetKind::Command { when_files, .. } => Some(when_files.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn markers_match(when_files: &[String], found: &BTreeSet<String>) -> bool {
    when_files.is_empty() || when_files.iter().any(|f| found.contains(f))
}
