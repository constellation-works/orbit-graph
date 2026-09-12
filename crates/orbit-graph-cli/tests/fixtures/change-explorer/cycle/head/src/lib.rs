pub fn is_even(n: u32) -> bool {
    if n == 0 {
        n == 0
    } else {
        is_odd(n - 1)
    }
}

pub fn is_odd(n: u32) -> bool {
    if n == 0 {
        false
    } else {
        is_even(n - 1)
    }
}
