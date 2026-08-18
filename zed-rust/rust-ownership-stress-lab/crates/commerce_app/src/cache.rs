use std::{collections::HashMap, rc::Rc};

use crate::domain::{Product, Sku};

pub struct ProductCache {
    entries: HashMap<Sku, Rc<Product>>,
    recent_keys: Vec<Sku>,
}

impl ProductCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            recent_keys: Vec::new(),
        }
    }

    pub fn insert(&mut self, product: Product) {
        self.recent_keys.push(product.sku.clone());
        self.entries.insert(product.sku.clone(), Rc::new(product));
    }

    /// E0594: Rc shares ownership but does not grant mutation.
    pub fn decrement_cached_stock(&self, sku: &Sku) {
        let product = self.entries.get(sku).expect("cached product");
        product.stock -= 1;
    }

    /// E0502: retaining mutably while an element reference is still live.
    pub fn compact_recent(&mut self) {
        let newest = self.recent_keys.last().expect("recent product");
        self.recent_keys
            .retain(|sku| self.entries.contains_key(sku));
        println!("newest retained product: {newest}");
    }

    /// E0502: removing the entry would invalidate the borrowed product name.
    pub fn evict_while_viewed(&mut self, sku: &Sku) {
        let product_name = &self.entries.get(sku).expect("cached product").name;
        self.entries.remove(sku);
        println!("evicted cached product: {product_name}");
    }

    /// E0594: cloning an Rc adds an owner, but neither owner may mutate directly.
    pub fn rename_shared_product(&self, sku: &Sku) {
        let shared = Rc::clone(self.entries.get(sku).expect("cached product"));
        shared.name = String::from("renamed");
    }

    /// E0499: two mutable references into the same recency list overlap.
    pub fn overwrite_recent_ends(&mut self) {
        let first = self.recent_keys.first_mut().expect("first key");
        let last = self.recent_keys.last_mut().expect("last key");
        *first = last.clone();
    }
}
