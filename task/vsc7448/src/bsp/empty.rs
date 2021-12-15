use crate::{vsc7448_spi::Vsc7448Spi, VscError};

/// Dummy default struct, which panics if ever used.
pub struct Bsp {}
impl Bsp {
    pub fn new(_vsc7448: &Vsc7448Spi) -> Result<Self, VscError> {
        panic!("No implementation for this board")
    }
    pub fn run(&self) -> ! {
        panic!("No implementation for this board")
    }
}
