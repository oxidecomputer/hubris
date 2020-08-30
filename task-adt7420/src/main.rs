#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use userlib::*;
use drv_i2c_api::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

const ADT7420_ADDRESS: u8 = 0x48;
const ADT7420_ID: u8 = 0xcb;

enum Register {
    TempMSB = 0x00,
    TempLSB = 0x01,
    Status = 0x02,
    Configuration = 0x03,
    THighMSB = 0x04,
    THighLSB = 0x05,
    TLowMSB = 0x06,
    TLowLSB = 0x07,
    TCritMSB = 0x08,
    TCritLSB = 0x09,
    THyst = 0x0a,
    ID = 0x0b,
}

fn validate(i2c: &I2c) -> bool {
    match i2c.read_reg::<u8, u8>(Register::ID as u8) {
        Ok(id) if id == ADT7420_ID => {
            sys_log!("adt7420: detected!");
            true
        }
        Ok(id) => {
            sys_log!("adt7420: incorrect ID {:x}", id);
            false
        }
        Err(err) => {
            sys_log!("adt7420: failed to read ID: {:?}", err);
            false
        }
    }
}

//
// Converts a tuple of two u8s (an MSB and an LSB) comprising a 13-bit value
// into a signed, floating point Celsius temperature value.  (This has been
// validated and verified against the sample data in Table 5 of the ADT7420
// datasheet.)
//
fn convert_temp13(raw: (u8, u8)) -> f32 {
    let msb = raw.0;
    let lsb = raw.1;
    let val = ((msb & 0x7f) as u16) << 5 | ((lsb >> 3) as u16);

    if msb & 0b1000_0000 != 0 {
        (val as i16 - 4096) as f32 / 16.0
    } else {
        val as f32 / 16.0
    }
}

fn convert_fahrenheit(temp: f32) -> f32 {
    temp * (9.0 / 5.0) + 32.0
}

fn read_temp(i2c: &I2c) {
    match i2c.read_reg::<u8, [u8; 2]>(Register::TempMSB as u8) {
        Ok(buf) => {
            let temp = convert_temp13((buf[0], buf[1]));
            let f = convert_fahrenheit(temp);
            sys_log!("adt7420: temp is {} degrees C, {} degrees F", temp, f);
        }
        Err(err) => {
            sys_log!("adt7420: failed to read temp: {:?}", err);
        }
    };
}

#[export_name = "main"]
fn main() -> ! {
    #[cfg(feature = "h7b3")]
    const INTERFACE: Interface = Interface::I2C4;

    let i2c = I2c::new(
        TaskId::for_index_and_gen(I2C as usize, Generation::default()),
        INTERFACE,
        ADT7420_ADDRESS
    );

    let mut configured = false;

    loop {
        hl::sleep_for(1000);

        if !configured {
            if !validate(&i2c) {
                continue;
            }

            configured = true;
        }

        read_temp(&i2c);
    }
}
