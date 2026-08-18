use crate::domain::Order;

pub struct MessageBus {
    subscribers: Vec<String>,
}

impl MessageBus {
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// E0596: mutation through an immutable receiver.
    pub fn subscribe(&self, topic: impl Into<String>) {
        self.subscribers.push(topic.into());
    }
}

/// E0502: clearing the queue invalidates the borrowed first message.
pub fn clear_queue_while_reading(mut queue: Vec<Order>) {
    let first = &queue[0];
    queue.clear();
    println!("discarded queued order {}", first.id);
}

impl MessageBus {
    /// E0596: removing a subscription also mutates through `&self`.
    pub fn unsubscribe(&self, topic: &str) {
        self.subscribers.retain(|subscriber| subscriber != topic);
    }

    /// E0502: replacing an element invalidates the borrowed topic view.
    pub fn replace_topic_while_reading(&mut self) {
        let previous = &self.subscribers[0];
        self.subscribers[0] = String::from("orders.updated");
        println!("replaced topic: {previous}");
    }
}

/// E0382: the first delivery owns the message, so a second delivery cannot reuse it.
pub fn publish_twice(order: Order) {
    deliver(order);
    deliver(order);
}

fn deliver(order: Order) {
    println!("delivering order {}", order.id);
}
