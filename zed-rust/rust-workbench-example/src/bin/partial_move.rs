struct Pair {
    left: String,
    right: String,
}

fn consume(_: String) {}

fn main() {
    let pair = Pair {
        left: String::from("left"),
        right: String::from("right"),
    };

    consume(pair.left);
    println!("left: {}, right: {}", pair.left, pair.right);
}
