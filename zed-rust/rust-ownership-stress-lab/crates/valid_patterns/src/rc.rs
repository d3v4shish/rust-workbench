use std::rc::Rc;

pub fn shared_readers() -> usize {
    let catalog = Rc::new(vec!["book", "video", "course"]);
    let search = Rc::clone(&catalog);
    let recommendations = Rc::clone(&catalog);
    search.len() + recommendations.len()
}
