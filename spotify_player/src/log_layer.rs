use std::{collections::VecDeque, fmt::Write, sync::Arc};

use parking_lot::Mutex;
use tracing::Subscriber;
use tracing_subscriber::Layer;

use crate::{
    config::AlertLevel,
    state::{Alert, AlertQueue},
};

pub struct BufferLayer {
    buffer: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl BufferLayer {
    pub fn new(buffer: Arc<Mutex<VecDeque<String>>>, max_lines: usize) -> Self {
        Self { buffer, max_lines }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        let level = event.metadata().level();
        let target = event.metadata().target();
        let mut line = format!(
            "{} {:>5} {}: {}",
            chrono::Local::now().format("%H:%M:%S"),
            level,
            target,
            visitor.message
        );
        for (name, value) in &visitor.fields {
            let _ = write!(line, " {name}={value}");
        }

        let mut buf = self.buffer.lock();
        buf.push_back(line);
        while buf.len() > self.max_lines {
            buf.pop_front();
        }
    }
}

/// A layer that turns warning/error log events into dismissible alerts,
/// so failures the application recovers from silently are still surfaced.
pub struct AlertLayer {
    alerts: Arc<Mutex<AlertQueue>>,
    min_level: AlertLevel,
}

impl AlertLayer {
    pub fn new(alerts: Arc<Mutex<AlertQueue>>, min_level: AlertLevel) -> Self {
        Self { alerts, min_level }
    }
}

impl<S: Subscriber> Layer<S> for AlertLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => AlertLevel::Error,
            tracing::Level::WARN => AlertLevel::Warn,
            _ => return,
        };
        if level.severity() < self.min_level.severity() {
            return;
        }

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        if visitor.message.is_empty() {
            return;
        }

        // must not log while holding the lock: this layer would then deadlock on itself
        self.alerts.lock().push(Alert::new(
            level,
            event.metadata().target().to_string(),
            visitor.message,
            visitor.fields,
        ));
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl EventVisitor {
    fn record(&mut self, field: &tracing::field::Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl tracing::field::Visit for EventVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn core::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.record(field, value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record(field, value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertLayer, AlertQueue};
    use crate::config::AlertLevel;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;

    /// Collect the alerts raised by `emit` through a subscriber using `min_level`.
    fn alerts_from(min_level: AlertLevel, emit: impl FnOnce()) -> Arc<Mutex<AlertQueue>> {
        let alerts = Arc::new(Mutex::new(AlertQueue::default()));
        let subscriber =
            tracing_subscriber::registry().with(AlertLayer::new(Arc::clone(&alerts), min_level));
        tracing::subscriber::with_default(subscriber, emit);
        alerts
    }

    #[test]
    fn raises_an_alert_with_the_event_fields() {
        let alerts = alerts_from(AlertLevel::Warn, || {
            tracing::warn!(
                client = "ncspot",
                retry_after_secs = 30,
                "Spotify Web API rate limit encountered"
            );
            tracing::info!("not an alert");
        });

        let alerts = alerts.lock();
        assert_eq!(alerts.len(), 1);
        let alert = alerts.current().unwrap();
        assert_eq!(alert.level, AlertLevel::Warn);
        assert_eq!(alert.message, "Spotify Web API rate limit encountered");
        assert_eq!(
            alert.fields,
            vec![
                ("client".to_string(), "ncspot".to_string()),
                ("retry_after_secs".to_string(), "30".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_events_below_the_configured_level() {
        let alerts = alerts_from(AlertLevel::Error, || {
            tracing::warn!("Spotify Web API rate limit encountered");
            tracing::error!("Failed to handle client request");
        });

        let alerts = alerts.lock();
        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts.current().unwrap().message,
            "Failed to handle client request"
        );
    }
}
