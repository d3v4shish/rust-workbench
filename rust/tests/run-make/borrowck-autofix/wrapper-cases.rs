use std::rc::Rc;

fn box_to_rc() {
    let values: Box<Vec<i32>> = Box::new(vec![1]);
    let shared = values;
    if shared.len() != 1 || values.len() != 1 {
        std::process::exit(1);
    }
}

fn plain_to_ref_cell() {
    let values: Vec<i32> = Vec::new();
    values.push(2);
    if values.len() != 1 {
        std::process::exit(2);
    }
}

fn box_to_rc_ref_cell() {
    let values: Box<Vec<i32>> = Box::new(Vec::new());
    values.push(3);
    if values.len() != 1 {
        std::process::exit(3);
    }
}

fn rc_to_rc_ref_cell() {
    let values: Rc<Vec<i32>> = Rc::new(Vec::new());
    values.push(4);
    if values.len() != 1 {
        std::process::exit(4);
    }
}

fn main() {
    box_to_rc();
    plain_to_ref_cell();
    box_to_rc_ref_cell();
    rc_to_rc_ref_cell();
}
