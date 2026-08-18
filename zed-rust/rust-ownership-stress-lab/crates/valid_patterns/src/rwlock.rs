use std::sync::{Arc, RwLock};

pub fn read_heavy_configuration() -> usize {
    let configuration = Arc::new(RwLock::new(vec!["safe", "fast"]));
    let reader = Arc::clone(&configuration);
    let observed = reader.read().unwrap().len();
    configuration.write().unwrap().push("documented");
    observed + configuration.read().unwrap().len()
}
