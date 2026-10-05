pub fn leaf() {}
pub fn left() { leaf(); }
pub fn right() { leaf(); }
pub fn root() { left(); right(); }
pub fn cycle_a() { cycle_b(); }
pub fn cycle_b() { cycle_a(); }
pub fn unicode() -> &'static str { "café 🪐" }
