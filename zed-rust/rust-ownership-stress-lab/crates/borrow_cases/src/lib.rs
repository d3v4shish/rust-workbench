//! Overlapping-loan failures produced by otherwise ordinary application code.

pub mod assign_while_viewed;
pub mod move_while_viewed;
pub mod read_then_write;
pub mod two_writers;
pub mod vec_reallocation;
