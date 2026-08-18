fn main() {
    let values: Box<Vec<i32>> = Box::new(Vec::new());

    values.push(1);
    println!("items: {}", values.len());
}
