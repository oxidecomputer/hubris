//! Driver for the DS2482-100 1-wire initiator

use bitfield::bitfield;
use drv_i2c_api::*;
use ringbuf::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Command {
    OneWireTriplet = 0x78,
    OneWireSingleBit = 0x87,
    OneWireReadByte = 0x96,
    OneWireWriteByte = 0xa5,
    OneWireReset = 0xb4,
    WriteConfiguration = 0xd2,
    SetReadPointer = 0xe1,
    DeviceReset = 0xf0,
}

bitfield! {
    pub struct Configuration(u8);
    onewire_speed, set_onewire_speed: 3;
    strong_pullup, set_strong_pullup: 2;
    active_pullup, set_active_pullup: 0;
}

impl Configuration {
    fn transit(&self) -> u8 {
        (self.0 & 0xf) | (!self.0 & 0xf) << 4
    }
}

bitfield! {
    pub struct Status(u8);
    branch_direction_taken, _: 7;
    triplet_second_bit, _: 6;
    single_bit_result, _: 5;
    device_reset, _: 4;
    logic_level, _: 3;
    short_detected, _: 2;
    presence_pulse_detect, _: 1;
    onewire_busy, _: 0;
}

bitfield! {
    pub struct TripletDirection(u8);
    direction, set_direction: 7;
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Register {
    Status = 0xf0,
    ReadData = 0xe1,
    Configuration = 0xc3,
}

#[derive(Copy, Clone, Debug)]
pub enum Error {
    BadCommand { cmd: Command, code: ResponseCode },
    BadRegisterRead { reg: Register, code: ResponseCode },
}

ringbuf!(
    (
        Option<(Command, Option<Register>)>,
        Result<u8, ResponseCode>
    ),
    196,
    (None, Ok(0))
);

pub struct Ds2482 {
    i2c: I2c,
    branches: Option<(u64, u64)>,
}

impl core::fmt::Display for Ds2482 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ds2482: {}", &self.i2c)
    }
}

fn read_register(i2c: &I2c, register: Register) -> Result<u8, Error> {
    let cmd = Command::SetReadPointer;
    let rval = i2c.read_reg::<[u8; 2], u8>([cmd as u8, register as u8]);

    ringbuf_entry!((Some((cmd, Some(register))), rval));

    match rval {
        Ok(rval) => Ok(rval),
        Err(code) => Err(Error::BadRegisterRead {
            reg: register,
            code: code,
        }),
    }
}

fn send_command(
    i2c: &I2c,
    cmd: Command,
    payload: Option<u8>,
) -> Result<(), Error> {
    let rval = match payload {
        Some(payload) => i2c.write(&[cmd as u8, payload]),
        None => i2c.write(&[cmd as u8]),
    };

    match rval {
        Ok(_) => {
            ringbuf_entry!((Some((cmd, None)), Ok(0)));
            Ok(())
        }
        Err(code) => {
            ringbuf_entry!((Some((cmd, None)), Err(code)));
            Err(Error::BadCommand {
                cmd: cmd,
                code: code,
            })
        }
    }
}

fn triplet(i2c: &I2c, take: bool) -> Result<(bool, bool), Error> {
    let mut payload = TripletDirection(0);
    payload.set_direction(take);

    send_command(i2c, Command::OneWireTriplet, Some(payload.0))?;

    loop {
        let status = Status(read_register(i2c, Register::Status)?);

        if status.onewire_busy() {
            continue;
        }

        return Ok((
            status.branch_direction_taken(),
            status.single_bit_result() == status.triplet_second_bit(),
        ));
    }
}

impl Ds2482 {
    pub fn new(i2c: &I2c) -> Self {
        Self {
            i2c: *i2c,
            branches: None,
        }
    }

    pub fn poll_until_notbusy(&self) -> Result<(), Error> {
        let i2c = &self.i2c;

        loop {
            let status = Status(read_register(i2c, Register::Status)?);

            if !status.onewire_busy() {
                return Ok(());
            }
        }
    }

    pub fn reset(&self) -> Result<(), Error> {
        let i2c = &self.i2c;

        self.poll_until_notbusy()?;

        send_command(i2c, Command::OneWireReset, None)?;
        self.poll_until_notbusy()?;

        Ok(())
    }

    pub fn initialize(&self) -> Result<(), Error> {
        let i2c = &self.i2c;

        send_command(i2c, Command::DeviceReset, None)?;

        let mut config = Configuration(0);
        config.set_active_pullup(true);

        send_command(i2c, Command::WriteConfiguration, Some(config.transit()))?;
        read_register(i2c, Register::Configuration)?;

        Ok(())
    }

    pub fn search(&mut self) -> Result<Option<u64>, Error> {
        let i2c = &self.i2c;

        let branches = match self.branches {
            Some(branches) => {
                if branches.0 == 0 {
                    self.branches = None;
                    return Ok(None);
                }
                branches
            }
            None => (0, 0),
        };

        let (id, nbranches) = drv_onewire::search(
            || {
                self.reset()?;
                let search = drv_onewire::Command::SearchROM as u8;
                send_command(i2c, Command::OneWireWriteByte, Some(search))?;
                self.poll_until_notbusy()?;

                Ok(())
            },
            |take| triplet(i2c, take),
            branches,
        )?;

        self.branches = Some(nbranches);

        Ok(Some(id))
    }

    pub fn write_byte(&self, byte: u8) -> Result<(), Error> {
        self.poll_until_notbusy()?;
        send_command(&self.i2c, Command::OneWireWriteByte, Some(byte))?;

        Ok(())
    }

    pub fn read_byte(&self) -> Result<u8, Error> {
        self.poll_until_notbusy()?;
        send_command(&self.i2c, Command::OneWireReadByte, None)?;

        self.poll_until_notbusy()?;
        let rval = read_register(&self.i2c, Register::ReadData)?;

        Ok(rval)
    }
}
