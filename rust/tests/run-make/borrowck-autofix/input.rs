fn consume(_: String) {}

fn main() {
    let values = Vec::new();
    values.push(1);

    let value = String::from("hello");
    consume(value);
    assert_eq!(value, "hello");
    assert_eq!(values, [1]);
}
