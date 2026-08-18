//! Compiler-validated shared-mutable wrapper candidates for a boxed collection.

fn main() {
    let queue: Box<Vec<String>> = Box::new(vec![String::from("first")]);
    queue.push(String::from("second"));
    println!("queue length: {}", queue.len());
}
