use std::{
    sync::{Arc, Mutex},
    thread,
};

pub fn shared_thread_counter() -> usize {
    let counter = Arc::new(Mutex::new(0));
    let handles = (0..4)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || *counter.lock().unwrap() += 1)
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }
    let final_count = *counter.lock().unwrap();
    final_count
}
