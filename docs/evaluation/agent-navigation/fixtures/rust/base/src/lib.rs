// Synthetic navigation fixture; direct module-local calls only.
pub fn normalize(value: i32) -> i32 {
    value.abs()
}

pub fn price(value: i32) -> i32 {
    normalize(value) * 2
}

pub fn invoice(value: i32) -> i32 {
    price(value) + 1
}

pub fn preview(value: i32) -> i32 {
    normalize(value)
}

pub fn price_preview(value: i32) -> i32 {
    value + 100
}

#[test]
fn test_invoice() {
    assert_eq!(invoice(-2), 7);
}

#[test]
fn test_preview() {
    assert_eq!(preview(-2), 2);
}

// External runtime consumers are not supplied in this source tree.
