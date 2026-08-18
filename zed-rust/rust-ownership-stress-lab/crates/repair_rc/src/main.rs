//! Compiler-validated `Box<T>` to `Rc<T>`/`Arc<T>` candidates.

fn main() {
    let measurements: Box<Vec<i64>> = Box::new(vec![10, 20, 30, 40]);
    let chart_data = measurements;

    println!("chart contains {} samples", chart_data.len());
    println!("table contains {} samples", measurements.len());
}
