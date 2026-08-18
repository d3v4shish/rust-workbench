use crate::{
    catalog::Catalog,
    domain::{Money, Order},
};

pub struct CheckoutService<'catalog> {
    catalog: &'catalog mut Catalog,
    completed_orders: Vec<String>,
}

impl<'catalog> CheckoutService<'catalog> {
    pub fn new(catalog: &'catalog mut Catalog) -> Self {
        Self {
            catalog,
            completed_orders: Vec::new(),
        }
    }

    pub fn preview(&self, order: &Order) -> Money {
        order.total()
    }

    /// E0382: the order transfers to persistence and is then reused for email.
    pub fn complete(&mut self, order: Order) {
        self.persist(order);
        self.send_receipt(&order);
    }

    fn persist(&mut self, order: Order) {
        self.completed_orders.push(order.id);
    }

    fn send_receipt(&self, order: &Order) {
        println!("receipt for {} to {}", order.id, order.customer.email);
    }

    /// E0382 on one branch: ownership conditionally transfers into manual review.
    pub fn route(&mut self, order: Order, requires_review: bool) {
        if requires_review {
            self.manual_review(order);
        }
        println!("routed order {}", order.id);
    }

    fn manual_review(&self, order: Order) {
        println!("reviewing {}", order.id);
    }

    /// E0382: archival consumes the order before its total is requested.
    pub fn archive_then_total(&self, order: Order) -> Money {
        self.manual_review(order);
        order.total()
    }

    /// E0382 partial move: the customer is extracted before using the whole order.
    pub fn detach_customer_then_price(&self, order: Order) -> Money {
        let customer = order.customer;
        println!("detached customer: {}", customer.name);
        order.total()
    }

    /// E0506: notes are replaced while a view into the old text is still live.
    pub fn replace_notes_while_viewed(&self, mut order: Order) {
        let first_word = order.notes.split_whitespace().next().unwrap_or("empty");
        order.notes = String::from("reviewed");
        println!("old note began with: {first_word}");
    }
}
