struct Profile {
    name: String,
    visits: u32,
}

fn main() {
    let profile = Profile {
        name: String::from("Ferris"),
        visits: 3,
    };

    let name = profile.name;
    println!("moved field: {name}");
    println!("copy field remains usable: {}", profile.visits);
}
