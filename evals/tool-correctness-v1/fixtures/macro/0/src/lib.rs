macro_rules! invoke { () => { helper() }; }
pub fn helper() {}
pub fn entry() { invoke!(); }
