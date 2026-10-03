use std::str::FromStr;
pub struct Compass;
pub struct Ledger;
impl FromStr for Compass {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, Self::Err> { todo!() }
}
impl Compass {
    pub fn parse<T>(value: T) -> T { value }
}
impl Ledger {
    pub fn parse(value: u8) -> u8 { value }
}
pub fn parse<T: Copy>(value: T) -> T { value }
pub mod nested {
    pub struct Dial;
    impl Dial {
        pub fn turn() {}
    }
    pub fn parse() {}
}
#[path = "relocated.rs"]
pub mod wire;
// fn counterfeit() {}
pub fn strings() {
    let text = r###"impl Ghost { fn counterfeit() {} }"###;
    /* outer /* fn counterfeit() {} */ comment */
}
