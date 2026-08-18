//! Rc supplies shared ownership, not shared mutable access.

use std::rc::Rc;

pub fn update_shared_cache() {
    let cache = Rc::new(vec![String::from("alpha")]);
    let worker_cache = Rc::clone(&cache);

    worker_cache.push(String::from("beta"));
    println!("cache: {cache:?}");
}
