pub fn subtotal(value: i32) -> i32 { value * 3 }
pub fn checkout(value: i32) -> i32 { subtotal(value) }
pub fn discount(value: i32) -> i32 { value - 1 }
