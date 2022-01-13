use crate::{spi::Vsc7448Spi, VscError};

pub enum Mode {
    Sgmii,
}
pub struct Config {
    // Nothing in here
}
impl Config {
    pub fn new(m: Mode) -> Self {
        unimplemented!()
    }
    pub fn apply(&self, instance: u32, v: &Vsc7448Spi) -> Result<(), VscError> {
        unimplemented!()
    }
}
