struct Pair {
    left: String,
    right: String,
}

fn consume(_: String) {}

fn main() {
    let count: i32 = 1;
    let copied = count;
    assert_eq!(count + copied, 2);

    let mut text = String::from("hello");
    let shared = &text;
    let _ = shared.len();
    text.push('!');

    let mut pair = Pair { left: String::from("left"), right: String::from("right") };
    consume(pair.left);
    pair.left = String::from("again");
    let moved = pair;
    consume(moved.left);
    consume(moved.right);
}
