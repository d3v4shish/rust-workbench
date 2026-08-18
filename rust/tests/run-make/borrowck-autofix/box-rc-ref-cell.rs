fn main() {
    let values: Box<Vec<i32>> = Box::new(Vec::new());
    values.push(1);
    assert_eq!(values.len(), 1);
}
