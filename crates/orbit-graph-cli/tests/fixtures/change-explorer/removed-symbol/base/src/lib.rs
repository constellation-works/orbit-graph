pub fn old_helper() -> i32 {
    42
}

pub fn caller() -> i32 {
    old_helper()
}
