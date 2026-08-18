//! The simplest mutation error. Often `mut` is better than introducing RefCell.

pub fn collect_metrics() {
    let metrics: Vec<u32> = Vec::new();
    metrics.push(60);
    metrics.push(75);
    println!("{metrics:?}");
}
