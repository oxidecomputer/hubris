// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_monorail_api::{Monorail, MonorailError};
use gateway_messages::vsc7448_port_status::{
    LinkStatus, PacketCount, PhyStatus, PhyType, PortConfig, PortCounters,
    PortDev, PortMode, PortSerdes, PortStatus, PortStatusError,
    PortStatusErrorCode, Speed,
};

pub(super) fn port_status(
    monorail: &Monorail,
    port: u32,
) -> Result<PortStatus, PortStatusError> {
    let (port_status, counters, phy_status) = monorail
        .get_port_status(port as u8)
        .and_then(|port_status| {
            monorail
                .get_port_counters(port as u8)
                .map(|counters| (port_status, counters))
        })
        .and_then(|(port_status, counters)| {
            match monorail.get_phy_status(port as u8) {
                Ok(phy_status) => Ok((port_status, counters, Some(phy_status))),
                Err(MonorailError::NoPhy) => Ok((port_status, counters, None)),
                Err(err) => Err(err),
            }
        })
        .map_err(|err| {
            let code = match err {
                MonorailError::UnconfiguredPort => {
                    PortStatusErrorCode::Unconfigured
                }
                _ => PortStatusErrorCode::Other(err as u32),
            };
            PortStatusError { port, code }
        })?;
    Ok(PortStatus {
        port,
        cfg: PortConfigConvert(port_status.cfg).into(),
        link_status: LinkStatusConvert(port_status.link_up).into(),
        phy_status: phy_status.map(|s| PhyStatusConvert(s).into()),
        counters: PortCountersConvert(counters).into(),
    })
}

struct PortConfigConvert(drv_monorail_api::PortConfig);

impl From<PortConfigConvert> for PortConfig {
    fn from(PortConfigConvert(s): PortConfigConvert) -> Self {
        Self {
            mode: PortModeConvert(s.mode).into(),
            dev: (PortDevConvert(s.dev.0).into(), s.dev.1),
            serdes: (PortSerdesConvert(s.serdes.0).into(), s.serdes.1),
        }
    }
}

struct PortDevConvert(drv_monorail_api::PortDev);

impl From<PortDevConvert> for PortDev {
    fn from(PortDevConvert(m): PortDevConvert) -> Self {
        match m {
            drv_monorail_api::PortDev::Dev1g => Self::Dev1g,
            drv_monorail_api::PortDev::Dev2g5 => Self::Dev2g5,
            drv_monorail_api::PortDev::Dev10g => Self::Dev10g,
        }
    }
}

struct PortSerdesConvert(drv_monorail_api::PortSerdes);

impl From<PortSerdesConvert> for PortSerdes {
    fn from(PortSerdesConvert(m): PortSerdesConvert) -> Self {
        match m {
            drv_monorail_api::PortSerdes::Serdes1g => Self::Serdes1g,
            drv_monorail_api::PortSerdes::Serdes6g => Self::Serdes6g,
            drv_monorail_api::PortSerdes::Serdes10g => Self::Serdes10g,
        }
    }
}

struct PortModeConvert(drv_monorail_api::PortMode);

impl From<PortModeConvert> for PortMode {
    fn from(PortModeConvert(m): PortModeConvert) -> Self {
        match m {
            drv_monorail_api::PortMode::Sfi => Self::Sfi,
            drv_monorail_api::PortMode::BaseKr => Self::BaseKr,
            drv_monorail_api::PortMode::Sgmii(s) => {
                Self::Sgmii(SpeedConvert(s).into())
            }
            drv_monorail_api::PortMode::Qsgmii(s) => {
                Self::Qsgmii(SpeedConvert(s).into())
            }
        }
    }
}

struct SpeedConvert(drv_monorail_api::Speed);

impl From<SpeedConvert> for Speed {
    fn from(SpeedConvert(s): SpeedConvert) -> Self {
        match s {
            drv_monorail_api::Speed::Speed100M => Self::Speed100M,
            drv_monorail_api::Speed::Speed1G => Self::Speed1G,
            drv_monorail_api::Speed::Speed10G => Self::Speed10G,
        }
    }
}

struct LinkStatusConvert(drv_monorail_api::LinkStatus);

impl From<LinkStatusConvert> for LinkStatus {
    fn from(LinkStatusConvert(s): LinkStatusConvert) -> Self {
        match s {
            drv_monorail_api::LinkStatus::Error => Self::Error,
            drv_monorail_api::LinkStatus::Down => Self::Down,
            drv_monorail_api::LinkStatus::Up => Self::Up,
        }
    }
}

struct PhyStatusConvert(drv_monorail_api::PhyStatus);

impl From<PhyStatusConvert> for PhyStatus {
    fn from(PhyStatusConvert(s): PhyStatusConvert) -> Self {
        Self {
            ty: PhyTypeConvert(s.ty).into(),
            mac_link_up: LinkStatusConvert(s.mac_link_up).into(),
            media_link_up: LinkStatusConvert(s.media_link_up).into(),
        }
    }
}

struct PhyTypeConvert(drv_monorail_api::PhyType);

impl From<PhyTypeConvert> for PhyType {
    fn from(PhyTypeConvert(t): PhyTypeConvert) -> Self {
        match t {
            drv_monorail_api::PhyType::Vsc8504 => Self::Vsc8504,
            drv_monorail_api::PhyType::Vsc8522 => Self::Vsc8522,
            drv_monorail_api::PhyType::Vsc8552 => Self::Vsc8552,
            drv_monorail_api::PhyType::Vsc8562 => Self::Vsc8562,
        }
    }
}

struct PortCountersConvert(drv_monorail_api::PortCounters);

impl From<PortCountersConvert> for PortCounters {
    fn from(PortCountersConvert(s): PortCountersConvert) -> Self {
        Self {
            tx: PacketCountConvert(s.tx).into(),
            rx: PacketCountConvert(s.rx).into(),
        }
    }
}

struct PacketCountConvert(drv_monorail_api::PacketCount);

impl From<PacketCountConvert> for PacketCount {
    fn from(PacketCountConvert(c): PacketCountConvert) -> Self {
        Self {
            multicast: c.multicast,
            unicast: c.unicast,
            broadcast: c.broadcast,
        }
    }
}
