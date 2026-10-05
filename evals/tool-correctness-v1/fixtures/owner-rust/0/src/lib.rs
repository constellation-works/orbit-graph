struct A;
struct B;
impl A { fn work() {} }
impl B { fn work() {} }
pub fn entry() { A::work(); }
pub fn other() { B::work(); }
