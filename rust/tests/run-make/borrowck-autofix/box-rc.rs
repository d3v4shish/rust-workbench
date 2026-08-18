fn main() {
    let values: Box<Vec<i32>> = Box::new(vec![1]);
    let shared = values;
    assert_eq!(shared.len(), 1);
    assert_eq!(values.len(), 1);
}
