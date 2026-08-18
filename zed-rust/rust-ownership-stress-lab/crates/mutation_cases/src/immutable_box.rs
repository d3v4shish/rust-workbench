//! A Box still has unique ownership; the binding simply lacks mutable access.

pub fn extend_heap_queue() {
    let queue: Box<Vec<String>> = Box::new(vec![String::from("first")]);
    queue.push(String::from("second"));
    println!("queue length: {}", queue.len());
}
