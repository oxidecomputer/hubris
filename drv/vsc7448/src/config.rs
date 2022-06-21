//! High-level configuration abstraction for the VSC7448

/// Port speed (only used for SGMII and QSGMII modes)
#[derive(Copy, Clone, Debug)]
pub enum Speed {
    Speed100M,
    Speed1G,
}

#[derive(Copy, Clone, Debug)]
pub enum PortMode {
    Sfi,
    Sgmii(Speed),
    Qsgmii(Speed),
}

#[derive(Copy, Clone, Debug)]
pub enum PortDev {
    Dev1g,
    Dev2g5,
    Dev10g,
}

#[derive(Copy, Clone, Debug)]
pub enum PortSerdes {
    Serdes1g,
    Serdes6g,
    Serdes10g,
}

#[derive(Copy, Clone, Debug)]
pub struct PortConfig {
    pub mode: PortMode,
    pub dev: (PortDev, usize),
    pub serdes: (PortSerdes, usize),
}

/// The VSC7448 has 52 physical ports.  The port mode uniquely determines the
/// port device type (1G, 2G5, etc) and device number.
#[derive(Copy, Clone, Debug)]
pub struct PortMap([PortMode; 52]);

impl PortMap {
    /// Decodes the configuration of the given port.
    ///
    /// # Special cases
    /// If the given port is assigned to QSGMII, the SERDES6G should only be
    /// configured for the first port in the group of four, but all four ports
    /// will return the SERDES number.
    ///
    /// If we're using a SERDES10G to run SGMII at a slower speed, special care
    /// must be taken to disable the associated DEV10G and configure the SERDES
    /// to run in slow mode.
    ///
    /// # Panics
    /// This will panic if i >= 52, or if the given port can't be configured in
    /// the requested mode.
    pub fn port_config(&self, p: usize) -> PortConfig {
        let mode = self.0[p];
        match mode {
            PortMode::Sfi => {
                let dev_num = match p {
                    49..=52 => p - 49,
                    _ => panic!("Invalid SFI port {}", p),
                };
                PortConfig {
                    mode,
                    dev: (PortDev::Dev10g, dev_num),
                    serdes: (PortSerdes::Serdes10g, dev_num),
                }
            }
            PortMode::Sgmii(_) => {
                let dev_type = match p {
                    0..=7 => PortDev::Dev1g,
                    8..=31 | 48..=52 => PortDev::Dev2g5,
                    _ => panic!("Invalid SGMII port {}", p),
                };
                let dev_num = match p {
                    0..=7 => p,
                    8..=31 => p - 8,
                    48..=52 => 24,
                    _ => unreachable!(), // checked above
                };
                // Note that port 48 is a DEV2G5 but uses SERDES1G instead of
                // SERDES6G - this is not a mistake!
                let serdes_type = match p {
                    0..=7 | 48 => PortSerdes::Serdes1g,
                    8..=31 => PortSerdes::Serdes6g,
                    49..=52 => PortSerdes::Serdes10g,
                    _ => unreachable!(), // checked above
                };
                // SERDES1G_1 maps to Port 0, SERDES1G_2 to Port 1, etc
                // SERDES6G_0 maps to Port 8, SERDES6G_1 to Port 9, etc
                // (notice that there's an offset here; SERDES1G_0 is used by
                //  the NPI port, i.e. port 48)
                let serdes_num = match p {
                    0..=7 => p + 1,
                    8..=31 => p - 8,
                    48 => 0,
                    49..=52 => p - 49,
                    _ => unreachable!(), // checked above
                };
                PortConfig {
                    mode,
                    dev: (dev_type, dev_num),
                    serdes: (serdes_type, serdes_num),
                }
            }
            PortMode::Qsgmii(_) => {
                let (dev_type, dev_num) = match p {
                    0..=7 => (PortDev::Dev1g, p),
                    8..=31 => (PortDev::Dev2g5, p - 8),
                    32..=47 => (PortDev::Dev1g, p - 24),
                    _ => panic!("Invalid QSGMII port {}", p),
                };
                // Ports 0-3 use SERDES6G_4, 4-7 use SERDES6G_5, etc
                let serdes_num = (p / 4) + 4;
                PortConfig {
                    mode,
                    dev: (dev_type, dev_num),
                    serdes: (PortSerdes::Serdes6g, serdes_num),
                }
            }
        }
    }
}
