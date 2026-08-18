//! The shared view remains live because it is used after the mutation.

pub fn normalize_title() {
    let mut title = String::from("  Rust ownership  ");
    let original_view = &title;

    title.make_ascii_uppercase();
    println!("before: {original_view}; after: {title}");
}
