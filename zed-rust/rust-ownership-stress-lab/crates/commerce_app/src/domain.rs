use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Sku(String);

impl Sku {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for Sku {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Money {
    cents: i64,
}

impl Money {
    pub fn usd(cents: i64) -> Self {
        Self { cents }
    }

    pub fn zero() -> Self {
        Self { cents: 0 }
    }

    pub fn cents(self) -> i64 {
        self.cents
    }

    pub fn times(self, quantity: u32) -> Self {
        Self::usd(self.cents * i64::from(quantity))
    }
}

impl std::ops::Add for Money {
    type Output = Self;

    fn add(self, other: Self) -> Self::Output {
        Self::usd(self.cents + other.cents)
    }
}

#[derive(Clone, Debug)]
pub struct Address {
    pub street: String,
    pub city: String,
    pub postal_code: String,
}

impl Address {
    pub fn new(
        street: impl Into<String>,
        city: impl Into<String>,
        postal_code: impl Into<String>,
    ) -> Self {
        Self {
            street: street.into(),
            city: city.into(),
            postal_code: postal_code.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Customer {
    pub id: u64,
    pub name: String,
    pub email: String,
    pub shipping_address: Address,
}

impl Customer {
    pub fn new(
        id: u64,
        name: impl Into<String>,
        email: impl Into<String>,
        shipping_address: Address,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            email: email.into(),
            shipping_address,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Product {
    pub sku: Sku,
    pub name: String,
    pub price: Money,
    pub stock: u32,
}

impl Product {
    pub fn new(sku: Sku, name: impl Into<String>, price: Money, stock: u32) -> Self {
        Self {
            sku,
            name: name.into(),
            price,
            stock,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OrderLine {
    pub sku: Sku,
    pub quantity: u32,
    pub unit_price: Money,
}

impl OrderLine {
    pub fn new(sku: Sku, quantity: u32, unit_price: Money) -> Self {
        Self {
            sku,
            quantity,
            unit_price,
        }
    }

    pub fn total(&self) -> Money {
        self.unit_price.times(self.quantity)
    }
}

#[derive(Clone, Debug)]
pub struct Order {
    pub id: String,
    pub customer: Customer,
    pub lines: Vec<OrderLine>,
    pub notes: String,
}

impl Order {
    pub fn new(id: impl Into<String>, customer: Customer, lines: Vec<OrderLine>) -> Self {
        Self {
            id: id.into(),
            customer,
            lines,
            notes: String::new(),
        }
    }

    pub fn total(&self) -> Money {
        self.lines
            .iter()
            .fold(Money::zero(), |total, line| total + line.total())
    }
}
