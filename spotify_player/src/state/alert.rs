use std::{collections::VecDeque, fmt::Write};

use chrono::{DateTime, Local};

use crate::config::AlertLevel;

/// Maximum number of pending alerts. Older alerts are dropped once the queue is full.
const MAX_ALERTS: usize = 20;

/// A user-facing notification about a warning or an error that would otherwise
/// only show up in the application's logs.
#[derive(Clone, Debug)]
pub struct Alert {
    /// Alerts sharing a key are coalesced into a single entry.
    key: String,
    pub level: AlertLevel,
    pub target: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
    pub count: usize,
    pub first_seen: DateTime<Local>,
    pub last_seen: DateTime<Local>,
}

impl Alert {
    pub fn new(
        level: AlertLevel,
        target: String,
        message: String,
        fields: Vec<(String, String)>,
    ) -> Self {
        let now = Local::now();
        Self {
            key: format!("{target}:{message}"),
            level,
            target,
            message,
            fields,
            count: 1,
            first_seen: now,
            last_seen: now,
        }
    }

    /// The alert as plain text, used when copying it to the clipboard.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "[{}] {} {}: {}",
            self.first_seen.format("%Y-%m-%d %H:%M:%S"),
            self.level,
            self.target,
            self.message
        );
        for (name, value) in &self.fields {
            let _ = write!(text, "\n  {name}: {value}");
        }
        if self.count > 1 {
            let _ = write!(
                text,
                "\n  seen {} times, last at {}",
                self.count,
                self.last_seen.format("%Y-%m-%d %H:%M:%S")
            );
        }
        text
    }
}

/// Pending alerts, oldest first.
#[derive(Debug, Default)]
pub struct AlertQueue {
    alerts: VecDeque<Alert>,
}

impl AlertQueue {
    /// Add an alert, coalescing it into an existing alert reporting the same event.
    pub fn push(&mut self, alert: Alert) {
        if let Some(existing) = self.alerts.iter_mut().find(|a| a.key == alert.key) {
            existing.count += 1;
            existing.last_seen = alert.last_seen;
            // keep the most recent field values (e.g. the latest `Retry-After`)
            existing.fields = alert.fields;
            return;
        }

        if self.alerts.len() >= MAX_ALERTS {
            self.alerts.pop_front();
        }
        self.alerts.push_back(alert);
    }

    /// The alert currently shown in the alert popup.
    pub fn current(&self) -> Option<&Alert> {
        self.alerts.front()
    }

    pub fn dismiss_current(&mut self) {
        self.alerts.pop_front();
    }

    pub fn dismiss_all(&mut self) {
        self.alerts.clear();
    }

    pub fn len(&self) -> usize {
        self.alerts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.alerts.is_empty()
    }

    /// All pending alerts as plain text, used when copying them to the clipboard.
    pub fn to_text(&self) -> String {
        self.alerts
            .iter()
            .map(Alert::to_text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{Alert, AlertQueue, MAX_ALERTS};
    use crate::config::AlertLevel;

    fn alert(message: &str) -> Alert {
        Alert::new(
            AlertLevel::Warn,
            "spotify_player::client".to_string(),
            message.to_string(),
            vec![],
        )
    }

    #[test]
    fn coalesces_repeated_alerts() {
        let mut queue = AlertQueue::default();
        queue.push(alert("rate limited"));
        queue.push(alert("rate limited"));
        queue.push(alert("other failure"));

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.current().unwrap().count, 2);
    }

    #[test]
    fn keeps_the_latest_fields_of_a_coalesced_alert() {
        let mut queue = AlertQueue::default();
        queue.push(alert("rate limited"));
        queue.push(Alert::new(
            AlertLevel::Warn,
            "spotify_player::client".to_string(),
            "rate limited".to_string(),
            vec![("retry_after_secs".to_string(), "30".to_string())],
        ));

        let current = queue.current().unwrap();
        assert_eq!(current.count, 2);
        assert_eq!(current.fields[0].1, "30");
    }

    #[test]
    fn drops_the_oldest_alert_when_full() {
        let mut queue = AlertQueue::default();
        for i in 0..=MAX_ALERTS {
            queue.push(alert(&format!("failure {i}")));
        }

        assert_eq!(queue.len(), MAX_ALERTS);
        assert_eq!(queue.current().unwrap().message, "failure 1");
    }

    #[test]
    fn dismisses_alerts() {
        let mut queue = AlertQueue::default();
        queue.push(alert("first"));
        queue.push(alert("second"));

        queue.dismiss_current();
        assert_eq!(queue.current().unwrap().message, "second");

        queue.dismiss_all();
        assert!(queue.is_empty());
    }
}
