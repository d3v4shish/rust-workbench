//! A function receiving `&Vec<T>` does not have permission to mutate its caller's vector.

fn append_default(values: &Vec<i32>) {
    values.push(0);
}

pub fn configure_defaults() {
    let values = vec![1, 2, 3];
    append_default(&values);
}
