//! Valid values for checking diagram families without a compiler error.

use std::{
    borrow::Cow,
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::{Rc, Weak},
    sync::{Arc, Mutex, RwLock},
};

pub trait Describe {
    fn describe(&self) -> String;
}

impl Describe for String {
    fn describe(&self) -> String {
        self.clone()
    }
}

pub fn nested_wrapper_shape() -> usize {
    let queue = Rc::new(RefCell::new(Vec::from([String::from("first")])));
    let observer: Weak<RefCell<Vec<String>>> = Rc::downgrade(&queue);
    queue.borrow_mut().push(String::from("second"));
    observer.upgrade().map_or(0, |queue| queue.borrow().len())
}

pub fn synchronized_wrapper_shape() -> usize {
    let shared = Arc::new(Mutex::new(Vec::from([1_usize, 2, 3])));
    let readable = Arc::new(RwLock::new(String::from("ready")));
    let count = shared.lock().expect("counter lock").len();
    count + readable.read().expect("status lock").len()
}

pub fn trait_object_shapes(value: &dyn Describe) -> (String, Box<dyn Describe>) {
    let borrowed_description = value.describe();
    let owned: Box<dyn Describe> = Box::new(String::from("owned trait object"));
    (borrowed_description, owned)
}

pub fn conditional_shape<'a>(borrowed: &'a str, own: bool) -> Cow<'a, str> {
    if own {
        Cow::Owned(borrowed.to_uppercase())
    } else {
        Cow::Borrowed(borrowed)
    }
}

pub fn closure_shape(prefix: String) -> impl FnMut(usize) -> String {
    let mut calls = 0;
    move |value| {
        calls += 1;
        format!("{prefix}: {value} (call {calls})")
    }
}

pub fn pinned_future_shape() -> Pin<Box<dyn Future<Output = usize> + Send>> {
    Box::pin(async {
        let before_await = String::from("stored in the future");
        std::future::ready(()).await;
        before_await.len()
    })
}

pub fn vec_reallocation_safe_shape(featured: &mut Vec<String>) {
    let first = featured[0].clone();
    featured.push(first.clone());
    println!("featured value duplicated: {first}");
}
