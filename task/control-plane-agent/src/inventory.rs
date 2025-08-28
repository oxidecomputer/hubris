// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use gateway_messages::measurement::{
    Measurement, MeasurementError, MeasurementKind,
};
use gateway_messages::sp_impl::{BoundsChecked, DeviceDescription};
use gateway_messages::{
    ComponentDetails, DeviceCapabilities, DevicePresence, SpComponent, SpError,
};
use task_sensor_api::Sensor as SensorTask;
use task_sensor_api::SensorError;
use task_validate_api::{Sensor, DEVICES as VALIDATE_DEVICES};
use task_validate_api::{Validate, ValidateError, ValidateOk};
use userlib::UnwrapLite;

userlib::task_slot!(VALIDATE, validate);
userlib::task_slot!(SENSOR, sensor);

pub(crate) struct Inventory {
    validate_task: Validate,
    sensor_task: SensorTask,
}

impl Inventory {
    pub(crate) fn new() -> Self {
        let () = devices_with_static_validation::ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET;

        Self {
            validate_task: Validate::from(VALIDATE.get_task_id()),
            sensor_task: SensorTask::from(SENSOR.get_task_id()),
        }
    }

    pub(crate) fn num_devices(&self) -> usize {
        OUR_DEVICES.len() + VALIDATE_DEVICES.len()
    }

    pub(crate) fn num_component_details(
        &self,
        component: &SpComponent,
    ) -> Result<u32, SpError> {
        match Index::try_from(component)? {
            Index::OurDevice(_) => Ok(0),
            Index::ValidateDevice(i) => {
                Ok(VALIDATE_DEVICES[i].sensors.len() as u32)
            }
        }
    }

    pub(crate) fn component_details(
        &self,
        component: &SpComponent,
        component_index: BoundsChecked,
    ) -> ComponentDetails {
        // `component_index` is guaranteed to be in the range
        // `0..num_component_details(component)`, and we only return a value
        // greater than 0 from that method for indices in the VALIDATE_DEVICES
        // range. We'll map the component back to an index back here and panic
        // for the unreachable branches (an out of range index or an index in
        // the `OurDevice(_)` subrange).
        let val_device_index = match Index::try_from(component) {
            Ok(Index::ValidateDevice(i)) => i,
            Ok(Index::OurDevice(_)) | Err(_) => panic!(),
        };

        let sensor_description = &VALIDATE_DEVICES[val_device_index].sensors
            [component_index.0 as usize];

        let value = self
            .sensor_task
            .get(sensor_description.id)
            .map_err(|err| SensorErrorConvert(err).into());

        ComponentDetails::Measurement(Measurement {
            name: sensor_description.name.unwrap_or(""),
            kind: MeasurementKindConvert(sensor_description.kind).into(),
            value,
        })
    }

    pub(crate) fn device_description(
        &self,
        index: BoundsChecked,
    ) -> DeviceDescription<'static> {
        // `index` is already bounds checked against our number of devices, so
        // we can call `from_overall_index` without worrying about a panic.
        let index = match Index::from_overall_index(index.0 as usize) {
            Index::OurDevice(i) => return OUR_DEVICES[i],
            Index::ValidateDevice(i) => i,
        };

        let device = &VALIDATE_DEVICES[index];

        let presence = match self.validate_task.validate_i2c(index as u32) {
            Ok(ValidateOk::Present | ValidateOk::Validated) => {
                DevicePresence::Present
            }
            Ok(ValidateOk::Removed) | Err(ValidateError::NotPresent) => {
                DevicePresence::NotPresent
            }
            Err(ValidateError::BadValidation) => DevicePresence::Failed,
            Err(ValidateError::Unavailable | ValidateError::DeviceOff) => {
                DevicePresence::Unavailable
            }
            Err(ValidateError::DeviceTimeout) => DevicePresence::Timeout,
            Err(ValidateError::InvalidDevice | ValidateError::DeviceError) => {
                DevicePresence::Error
            }
        };

        let mut capabilities = DeviceCapabilities::empty();
        if !device.sensors.is_empty() {
            capabilities |= DeviceCapabilities::HAS_MEASUREMENT_CHANNELS;
        }
        DeviceDescription {
            component: SpComponent { id: device.id },
            device: device.device,
            description: device.description,
            capabilities,
            presence,
        }
    }
}

// Our parent deals primarily in overall device indices (`0..num_devices()`),
// but internally we partition that into `[OUR_DEVICES | VALIDATE_DEVICES]`.
// This enum helps us avoid needing to mix adjustment between partitioned
// and not partitioned indices in `Inventory` above.
#[derive(Debug, Clone, Copy)]
enum Index {
    // A device described by the `OUR_DEVICES` array (i.e., special components
    // that we and MGS know about).
    OurDevice(usize),
    // A device described by the `VALIDATE_DEVICES` array (i.e., generic
    // components that are enumerated at compile time into validate-api).
    ValidateDevice(usize),
}

impl Index {
    /// Convert from an overall index (`0..num_devices()`) into our partitioned
    /// space.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is past the end of our total component count.
    fn from_overall_index(idx: usize) -> Self {
        if idx < OUR_DEVICES.len() {
            Self::OurDevice(idx)
        } else {
            let idx = idx - OUR_DEVICES.len();
            if idx < VALIDATE_DEVICES.len() {
                Self::ValidateDevice(idx)
            } else {
                panic!()
            }
        }
    }
}

impl TryFrom<&'_ SpComponent> for Index {
    type Error = SpError;

    fn try_from(component: &'_ SpComponent) -> Result<Self, Self::Error> {
        if let Ok(entry_idx) = task_validate_api::DEVICE_INDICES_BY_SORTED_ID
            .binary_search_by_key(&component.id, |&(id, _)| id)
        {
            let &(_, index) = task_validate_api::DEVICE_INDICES_BY_SORTED_ID
                .get(entry_idx)
                .unwrap_lite();
            return Ok(Self::ValidateDevice(index));
        }
        for (i, d) in OUR_DEVICES.iter().enumerate() {
            if *component == d.component {
                return Ok(Self::OurDevice(i));
            }
        }
        Err(SpError::RequestUnsupportedForComponent)
    }
}

use devices_with_static_validation::OUR_DEVICES;
// We tag this with module `#[allow(dead_code)]` to prevent warnings about the
// contents of this module not being used; it contains constants used in static
// assertion that are otherwise dead code.
#[allow(dead_code)]
mod devices_with_static_validation {
    use super::{
        DeviceCapabilities, DeviceDescription, DevicePresence, SpComponent,
    };
    use task_validate_api::DEVICES_CONST as VALIDATE_DEVICES_CONST;

    // List of logical or high-level components that this task is responsible
    // for (or at least responds to in terms of MGS requests for status /
    // update, even if another task is actually responsible for lower-level
    // details).
    //
    // TODO: Are our device names and descriptions good enough, or are there more
    //       specific names we should use? This may be answered when we expand
    //       DeviceDescription with any VPD / serial numbers.
    const OUR_DEVICES_CONST: &[DeviceDescription<'static>] = &[
        // We always include "ourself" as a component; this is the component name
        // MGS uses to send SP image updates.
        DeviceDescription {
            component: SpComponent::SP_ITSELF,
            device: SpComponent::SP_ITSELF.const_as_str(),
            description: "Service Processor",
            capabilities: DeviceCapabilities::UPDATEABLE,
            presence: DevicePresence::Present,
        },
        // If we have the auxflash feature enabled, report the auxflash as a
        // component. We do not mark it as explicitly "updateable", even though
        // it is written as a part of the SP update process. Crucially, that is
        // a part of updating the `SP_ITSELF` component; the auxflash is not
        // independently updateable.
        #[cfg(feature = "auxflash")]
        DeviceDescription {
            component: SpComponent::SP_AUX_FLASH,
            device: SpComponent::SP_AUX_FLASH.const_as_str(),
            description: "Service Processor auxiliary flash",
            capabilities: DeviceCapabilities::empty(),
            presence: DevicePresence::Present,
        },
        // If we're building for gimlet, we always claim to have a host CPU.
        //
        // This is a lie on gimletlet (where we still build with the "gimlet"
        // feature), but a useful one in general.
        #[cfg(feature = "gimlet")]
        DeviceDescription {
            component: SpComponent::SP3_HOST_CPU,
            device: SpComponent::SP3_HOST_CPU.const_as_str(),
            description: "Gimlet SP3 host cpu",
            capabilities: DeviceCapabilities::HAS_SERIAL_CONSOLE,
            presence: DevicePresence::Present, // TODO: ok to assume always present?
        },
        // Same for cosmo / grapefruit
        #[cfg(feature = "cosmo")]
        DeviceDescription {
            component: SpComponent::SP5_HOST_CPU,
            device: SpComponent::SP5_HOST_CPU.const_as_str(),
            description: "Cosmo SP5 host cpu",
            capabilities: DeviceCapabilities::HAS_SERIAL_CONSOLE,
            presence: DevicePresence::Present, // TODO: ok to assume always present?
        },
        // If we're building for gimlet, we always claim to have host boot flash.
        //
        // This is a lie on gimletlet (where we still build with the "gimlet"
        // feature), and a less useful one than the host CPU (since trying to
        // access the "host flash" will fail unless we have an adapter providing
        // QSPI flash).
        #[cfg(feature = "compute-sled")]
        DeviceDescription {
            component: SpComponent::HOST_CPU_BOOT_FLASH,
            device: SpComponent::HOST_CPU_BOOT_FLASH.const_as_str(),
            #[cfg(feature = "gimlet")]
            description: "Gimlet host boot flash",
            #[cfg(feature = "cosmo")]
            description: "Cosmo host boot flash",
            capabilities: DeviceCapabilities::UPDATEABLE,
            presence: DevicePresence::Present, // TODO: ok to assume always present?
        },
        // If we're building for sidecar, we always claim to have a monorail.
        #[cfg(feature = "sidecar")]
        DeviceDescription {
            component: SpComponent::MONORAIL,
            device: SpComponent::MONORAIL.const_as_str(),
            description: "Management network switch",
            capabilities: DeviceCapabilities::HAS_MEASUREMENT_CHANNELS,
            // Fine to assume this is always present; if it isn't, we can't respond
            // to MGS messages anyway!
            presence: DevicePresence::Present,
        },
        #[cfg(any(
            feature = "gimlet",
            feature = "cosmo",
            feature = "psc",
            feature = "sidecar"
        ))]
        DeviceDescription {
            component: SpComponent::SYSTEM_LED,
            device: SpComponent::SYSTEM_LED.const_as_str(),
            description: "System attention LED",
            capabilities: DeviceCapabilities::IS_LED,
            // The LED is soldered to the board
            presence: DevicePresence::Present,
        },
    ];

    pub(super) static OUR_DEVICES: &[DeviceDescription<'static>] =
        OUR_DEVICES_CONST;

    // We will spread the contents of `DEVICES` out over multiple packets to
    // MGS; however, we do _not_ currently handle the case where a single
    // `DEVICES` entry is too large to fit in a packet, even if it's the only
    // device present in that packet. Therefore, we assert at compile time via
    // all the machinery below that each entry of `DEVICES` is small enough that
    // it will indeed fit in one packet after being packed into a TLV triple.
    pub(super) const ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET: () =
        assert_each_device_tlv_fits_in_one_packet();

    const fn assert_device_tlv_fits_in_one_packet(
        device: &'static str,
        description: &'static str,
    ) {
        use gateway_messages::{tlv, SerializedSize, MIN_TRAILING_DATA_LEN};

        let encoded_len = tlv::tlv_len(
            gateway_messages::DeviceDescriptionHeader::MAX_SIZE
                + device.len()
                + description.len(),
        );

        if encoded_len > MIN_TRAILING_DATA_LEN {
            panic!(concat!(
                "The device details (device and description) of at least one ",
                "device in the current app.toml are too long to fit in a ",
                "single TLV triple to send to MGS. Current Rust restrictions ",
                "prevent us from being able to specific the specific device ",
                "in this error message. Change this panic to ",
                "`panic!(\"{{}}\", description)` and rebuild to see the ",
                "description of the too-long device instead."
            ));
        }
    }

    const fn assert_each_device_tlv_fits_in_one_packet() {
        // Check devices described by `validate`.
        let mut i = 0;
        loop {
            if i == VALIDATE_DEVICES_CONST.len() {
                break;
            }
            assert_device_tlv_fits_in_one_packet(
                VALIDATE_DEVICES_CONST[i].device,
                VALIDATE_DEVICES_CONST[i].description,
            );
            i += 1;
        }

        // Check devices described by us.
        let mut i = 0;
        loop {
            if i == OUR_DEVICES_CONST.len() {
                break;
            }
            assert_device_tlv_fits_in_one_packet(
                OUR_DEVICES_CONST[i].device,
                OUR_DEVICES_CONST[i].description,
            );
            i += 1;
        }
    }
}

struct MeasurementKindConvert(Sensor);

impl From<MeasurementKindConvert> for MeasurementKind {
    fn from(value: MeasurementKindConvert) -> Self {
        match value.0 {
            Sensor::Temperature => Self::Temperature,
            Sensor::Power => Self::Power,
            Sensor::Current => Self::Current,
            Sensor::Voltage => Self::Voltage,
            Sensor::InputCurrent => Self::InputCurrent,
            Sensor::InputVoltage => Self::InputVoltage,
            Sensor::Speed => Self::Speed,
        }
    }
}

struct SensorErrorConvert(SensorError);

impl From<SensorErrorConvert> for MeasurementError {
    fn from(value: SensorErrorConvert) -> Self {
        match value.0 {
            SensorError::NoReading => Self::NoReading,
            SensorError::NotPresent => Self::NotPresent,
            SensorError::DeviceError => Self::DeviceError,
            SensorError::DeviceUnavailable => Self::DeviceUnavailable,
            SensorError::DeviceTimeout => Self::DeviceTimeout,
            SensorError::DeviceOff => Self::DeviceOff,
        }
    }
}
