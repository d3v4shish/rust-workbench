//! Five deliberate non-ownership errors for the broader Rust Learning Debugger.

use std::rc::Rc;

pub fn type_mismatch() -> usize {
    let count: usize = "not a number";
    count
}

pub fn temporary_lifetime() -> &'static str {
    String::from("temporary").as_str()
}

pub fn return_local_reference() -> &'static String {
    let local = String::from("local");
    &local
}

pub fn rc_across_threads() {
    let shared = Rc::new(vec![1, 2, 3]);
    std::thread::spawn(move || println!("{shared:?}"));
}

pub fn method_not_available() {
    let count = 3_u32;
    count.push(4);
}

pub fn await_outside_async_context() {
    std::future::ready(String::from("done")).await;
}

pub async fn recursive_async_state(depth: usize) {
    if depth > 0 {
        recursive_async_state(depth - 1).await;
    }
}

pub fn closure_may_outlive_borrow() {
    let message = String::from("captured by reference");
    std::thread::spawn(|| println!("{message}"));
}
