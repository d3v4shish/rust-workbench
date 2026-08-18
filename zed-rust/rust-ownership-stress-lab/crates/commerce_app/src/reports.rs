use crate::domain::{Customer, Order};

pub fn customer_summary(customer: Customer) -> String {
    format!("{} <{}>", customer.name, customer.email)
}

/// E0382: extracting the customer by value prevents later whole-order access.
pub fn render_order(order: Order) -> String {
    let customer = order.customer;
    let summary = customer_summary(customer);
    format!("{}: {summary}; total={}", order.id, order.total().cents())
}

/// E0506: replacing a label invalidates the borrowed suffix.
pub fn replace_label_while_viewed(mut label: String) {
    let suffix = label.strip_prefix("report:").unwrap_or(&label);
    label = String::from("archived");
    println!("previous report label: {suffix}");
}

/// E0382: rendering consumes the order, so it cannot be rendered a second time.
pub fn render_two_formats(order: Order) -> (String, String) {
    let text = render_order(order);
    let html = render_order(order);
    (text, html)
}

/// E0502: clearing the title invalidates the borrowed prefix.
pub fn clear_title_while_viewed(mut title: String) {
    let prefix = title.split(':').next().unwrap_or("report");
    title.clear();
    println!("cleared report category: {prefix}");
}

/// E0382 partial move: moving email prevents passing the whole customer onward.
pub fn detach_email_then_summarize(customer: Customer) -> String {
    let email = customer.email;
    println!("detached email: {email}");
    customer_summary(customer)
}
