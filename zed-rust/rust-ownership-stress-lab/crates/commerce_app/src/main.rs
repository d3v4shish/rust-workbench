//! A deliberately broken but realistic commerce application.
//!
//! The ownership mistakes live in normal application modules rather than isolated snippets.

mod analytics;
mod cache;
mod catalog;
mod checkout;
mod domain;
mod fulfillment;
mod messaging;
mod reports;

use catalog::Catalog;
use checkout::CheckoutService;
use domain::{Address, Customer, Money, Order, OrderLine, Product, Sku};

fn seed_catalog() -> Catalog {
    Catalog::new(vec![
        Product::new(
            Sku::new("RUST-BOOK"),
            "The Rust Programming Language",
            Money::usd(3999),
            25,
        ),
        Product::new(
            Sku::new("FERRIS-PLUSH"),
            "Ferris plush toy",
            Money::usd(2499),
            10,
        ),
    ])
}

fn sample_order() -> Order {
    let customer = Customer::new(
        101,
        "Ada Lovelace",
        "ada@example.test",
        Address::new("12 Analytical Engine Way", "London", "SW1A 1AA"),
    );
    Order::new(
        "ORDER-2026-0001",
        customer,
        vec![
            OrderLine::new(Sku::new("RUST-BOOK"), 1, Money::usd(3999)),
            OrderLine::new(Sku::new("FERRIS-PLUSH"), 2, Money::usd(2499)),
        ],
    )
}

fn main() {
    let mut catalog = seed_catalog();
    let mut checkout = CheckoutService::new(&mut catalog);
    let order = sample_order();

    checkout.preview(&order);
    println!("Commerce ownership stress application is intentionally broken.");
    println!("Open TESTING.md and inspect each module independently.");
}
