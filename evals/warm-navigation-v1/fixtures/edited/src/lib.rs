pub fn helper(v: i32) -> i32 {
    v + 1
}

pub fn price(v: i32) -> i32 {
    discount(helper(v) * 3)
}

pub fn discount(v: i32) -> i32 {
    v - 2
}
