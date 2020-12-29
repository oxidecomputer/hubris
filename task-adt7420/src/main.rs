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
    let controller = i2c.controller;

    match i2c.read_reg::<u8, u8>(Register::ID as u8) {
        Ok(id) if id == ADT7420_ID => {
            sys_log!("adt7420: {:?}: detected!", bus);
            true
        }
        Ok(id) => {
            sys_log!("adt7420: {:?}: incorrect ID {:x}", bus, id);
            false
        }
        Err(err) => {
            sys_log!("adt7420: {:?}: failed to read ID: {:?}", bus, err);
            false
        }
    }
}

// Roll on const generics!
struct TempBufferBySecond {
    last: Option<usize>,
    values: [f32; 3600],
}

struct TempBufferByMinute {
    last: Option<usize>,
    values: [f32; 4320],
}

#[no_mangle]
static mut TEMPS_BYSECOND: TempBufferBySecond = TempBufferBySecond {
    last: None,
    values: [0.0; 3600]
};

#[no_mangle]
static mut TEMPS_BYMINUTE: TempBufferByMinute = TempBufferByMinute {
    last: None,
    values: [0.0; 4320]
};

fn store_temp_byminute(val: f32) {
    let mut temps = unsafe { &mut TEMPS_BYMINUTE };

    let index = match temps.last {
        None => 0,
        Some(val) => (val + 1) % temps.values.len(),
    };

    temps.values[index] = val;
    temps.last = Some(index);
}

fn store_temp_bysecond(val: f32) {
    let mut temps = unsafe { &mut TEMPS_BYSECOND };

    let index = match temps.last {
        None => 0,
        Some(val) => (val + 1) % temps.values.len(),
    };

    temps.values[index] = val;
    temps.last = Some(index);

    if index % 60 == 0 {
        store_temp_byminute(val);
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
    let controller = i2c.controller;

    match i2c.read_reg::<u8, [u8; 2]>(Register::TempMSB as u8) {
        Ok(buf) => {
            let temp = convert_temp13((buf[0], buf[1]));

            store_temp_bysecond(temp);

            let f = convert_fahrenheit(temp);

            // Avoid default formatting to save a bunch of text and stack
            sys_log!("adt7420: {:?}: temp is {}.{:03} degrees C, \
                {}.{:03} degrees F",
                interface,
                temp as i32, (((temp + 0.0005) * 1000.0) as i32) % 1000,
                f as i32, (((f + 0.0005) * 1000.0) as i32) % 1000);
        }
        Err(err) => {
            sys_log!("adt7420: {:?}: failed to read temp: {:?}", interface, err);
        }
    };
}

fn i2c(controller: Controller) -> (I2c, bool) {
    (I2c::new(
        TaskId::for_index_and_gen(I2C as usize, Generation::default()),
        controller,
        Port::Default,
        None,
        ADT7420_ADDRESS
    ), false)
}

#[export_name = "main"]
fn main() -> ! {
    let mut interfaces = [
        i2c(Interface::I2C1),
        i2c(Interface::I2C2),
        i2c(Interface::I2C3),
        i2c(Interface::I2C4),
    ];

    loop {
        hl::sleep_for(1000);

        for bus in &mut busses {
            if bus.1 {
                read_temp(&interface.0);
            } else {
                if validate(&interface.0) {
                    bus.1 = true;
                }
            }
        }
    }
}
