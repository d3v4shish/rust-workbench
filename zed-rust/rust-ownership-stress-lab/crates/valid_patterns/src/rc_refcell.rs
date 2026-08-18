use std::{cell::RefCell, rc::Rc};

pub fn shared_local_state() -> i32 {
    let score = Rc::new(RefCell::new(0));
    let ui_score = Rc::clone(&score);
    let game_score = Rc::clone(&score);
    *ui_score.borrow_mut() += 5;
    *game_score.borrow_mut() += 10;
    let final_score = *score.borrow();
    final_score
}
