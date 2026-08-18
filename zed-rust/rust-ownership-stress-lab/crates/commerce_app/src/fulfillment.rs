use crate::domain::{Address, Order};

pub struct Shipment {
    pub order_id: String,
    pub destination: Address,
    pub labels: Vec<String>,
}

/// E0382 partial move: fields move into a shipment before code logs the whole customer.
pub fn create_shipment(order: Order) -> Shipment {
    let shipment = Shipment {
        order_id: order.id,
        destination: order.customer.shipping_address,
        labels: vec![String::from("standard")],
    };
    println!("shipping for customer: {:?}", order.customer);
    shipment
}

/// E0505: a postal-code view is still live when the address is moved.
pub fn reroute(address: Address) -> Address {
    let postal_prefix = &address.postal_code[..2];
    let moved_address = address;
    println!("rerouted region: {postal_prefix}");
    moved_address
}

/// E0382: dispatch takes ownership before the confirmation message is built.
pub fn dispatch_then_confirm(order: Order) {
    dispatch(order);
    println!("confirmed dispatch for {}", order.id);
}

fn dispatch(order: Order) {
    println!("dispatching {}", order.id);
}

/// E0506: replacing an address invalidates the borrowed city view.
pub fn replace_address_while_viewed(mut address: Address, replacement: Address) -> Address {
    let previous_city = &address.city;
    address = replacement;
    println!("shipment moved away from {previous_city}");
    address
}

/// E0382 partial move: labels move out before a whole-shipment summary is requested.
pub fn detach_labels_then_summarize(shipment: Shipment) {
    let labels = shipment.labels;
    println!("detached {} labels", labels.len());
    summarize_shipment(&shipment);
}

fn summarize_shipment(shipment: &Shipment) {
    println!(
        "shipment {} has {} labels",
        shipment.order_id,
        shipment.labels.len()
    );
}
