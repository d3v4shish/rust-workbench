//! Compiler-validated `RefCell<T>`, `Mutex<T>`, and `RwLock<T>` candidates.

fn main() {
    let metrics: Vec<i32> = Vec::new();
    metrics.push(60);
    assert_eq!(metrics.len(), 1);
}
