use std::sync::{Arc, Mutex};

fn main() {
    let numbers = Arc::new(Mutex::new(vec![1, 2, 3]));
    let worker = Arc::clone(&numbers);
    worker.lock().unwrap().push(4);
    println!("locked shared value: {:?}", numbers.lock().unwrap());
}
