//! `push` may reallocate the vector, which would invalidate an element reference.

pub fn append_after_selecting() {
    let mut jobs = vec![String::from("parse"), String::from("compile")];
    let selected_job = &jobs[0];

    jobs.push(String::from("link"));
    println!("selected job remains {selected_job}");
}
