use std::collections::HashMap;

use crate::domain::{Money, Product, Sku};

pub struct Catalog {
    products: HashMap<Sku, Product>,
    featured: Vec<Sku>,
}

impl Catalog {
    pub fn new(products: Vec<Product>) -> Self {
        let featured = products.iter().map(|product| product.sku.clone()).collect();
        let products = products
            .into_iter()
            .map(|product| (product.sku.clone(), product))
            .collect();
        Self { products, featured }
    }

    pub fn get(&self, sku: &Sku) -> Option<&Product> {
        self.products.get(sku)
    }

    pub fn reserve(&mut self, sku: &Sku, quantity: u32) -> bool {
        let Some(product) = self.products.get_mut(sku) else {
            return false;
        };
        if product.stock < quantity {
            return false;
        }
        product.stock -= quantity;
        true
    }

    /// E0502: the immutable product view is used after reserving mutably.
    pub fn quote_and_reserve(&mut self, sku: &Sku, quantity: u32) -> Option<Money> {
        let product = self.products.get(sku)?;
        self.reserve(sku, quantity);
        println!("reserved product {}", product.name);
        Some(product.price.times(quantity))
    }

    /// E0499: two mutable entries from the same map overlap.
    pub fn transfer_stock(&mut self, from: &Sku, to: &Sku, quantity: u32) {
        let source = self.products.get_mut(from).expect("source product");
        let destination = self.products.get_mut(to).expect("destination product");
        source.stock -= quantity;
        destination.stock += quantity;
    }

    /// E0502: pushing may reallocate while a reference into the vector is live.
    pub fn duplicate_first_feature(&mut self) {
        let first = &self.featured[0];
        self.featured.push(first.clone());
        println!("featured product duplicated: {first}");
    }

    /// E0502: removing an entry invalidates the borrowed product name.
    pub fn evict_while_reporting(&mut self, sku: &Sku) {
        let name = &self.products.get(sku).expect("catalog product").name;
        self.products.remove(sku);
        println!("evicted product: {name}");
    }

    /// E0499: two mutable positions in the featured list overlap.
    pub fn rename_feature_ends(&mut self) {
        let first = self.featured.first_mut().expect("first feature");
        let last = self.featured.last_mut().expect("last feature");
        *first = last.clone();
    }
}
