mod a;
mod b;

pub fn call_a() -> i32 {
    a::run()
}

pub fn call_b() -> i32 {
    b::run()
}
