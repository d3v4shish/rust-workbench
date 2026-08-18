fn main() {
    let values: Box<Vec<i32>> = Box::new(vec![1, 2, 3]);
    let shared = values;

    println!("shared has {} items", shared.len());
    println!("original has {} items", values.len());
}
