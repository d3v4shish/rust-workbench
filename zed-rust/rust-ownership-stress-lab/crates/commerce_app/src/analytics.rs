use crate::domain::Order;

pub struct Analytics {
    events: Vec<String>,
}

impl Analytics {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// E0596: the method takes `&self` but writes to the event buffer.
    pub fn track_order(&self, order: &Order) {
        self.events
            .push(format!("order:{}:{}", order.id, order.total().cents()));
    }

    /// E0506: the borrowed prefix remains needed after replacing the event.
    pub fn redact_latest(&mut self) {
        let current = self.events.last_mut().expect("event");
        let prefix = &current[..current.find(':').unwrap_or(current.len())];
        *current = String::from("redacted");
        println!("redacted event category: {prefix}");
    }

    /// E0596: clearing the buffer also requires a mutable receiver.
    pub fn clear_history(&self) {
        self.events.clear();
    }

    /// E0502: appending may move the buffer while a view into it remains live.
    pub fn duplicate_latest(&mut self) {
        let latest = self.events.last().expect("latest event");
        self.events.push(latest.clone());
        println!("duplicated event: {latest}");
    }

    /// E0382: recording takes ownership before the caller logs the event.
    pub fn record_then_log(&self, event: String) {
        self.record_owned(event);
        println!("recorded event: {event}");
    }

    fn record_owned(&self, event: String) {
        println!("sending event to storage: {event}");
    }
}
