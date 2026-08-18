//! One field moves out of a struct. Rust still permits access to an unaffected field.

pub struct Profile {
    pub display_name: String,
    pub biography: String,
    pub login_count: u64,
}

fn index_name(name: String) {
    println!("indexed {name}");
}

pub fn update_search_index() {
    let profile = Profile {
        display_name: String::from("Ferris"),
        biography: String::from("Systems programmer"),
        login_count: 42,
    };

    index_name(profile.display_name);
    println!(
        "login count is still individually usable: {}",
        profile.login_count
    );
    println!(
        "whole profile: {} / {}",
        profile.display_name, profile.biography
    );
}
