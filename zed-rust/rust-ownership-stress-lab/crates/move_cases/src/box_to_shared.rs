//! A unique heap owner is moved, but the author appears to want two owners.

pub fn load_dashboard() {
    let measurements: Box<Vec<i64>> = Box::new(vec![10, 20, 30, 40]);
    let chart_data: Box<Vec<i64>> = measurements;

    println!("chart contains {} samples", chart_data.len());
    println!("table contains {} samples", measurements.len());
}
