//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! Mostly for demonstration purposes, write is verified read is not
//!
//! # IPC protocol
//!
//! ## `read` (1)
//!
//! Reads the buffer into lease #0. Returns when completed
//!
//!
//! ## `write` (2)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//! ## `exchange` (3)
//!
//! Sends the contents of lease #0 and writes received data into lease #1

#![no_std]
#![no_main]

use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = Task::anonymous;

#[repr(u32)]
#[derive(Debug)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
    Underrun = 4,
}

// Read/Write is defined from the perspective of the SPI device
#[derive(FromPrimitive, PartialEq)]
enum Op {
    Read = 1,
    Write = 2,
    Exchange = 3,
}

struct SpiState {
    task: hl::Caller<()>,
    len: usize,
    tx_pos: usize,
    tx_lease_num: usize,
    rx_pos: usize,
    rx_lease_num: usize,
}

// TODO: it is super unfortunate to have to write this by hand, but deriving
// ToPrimitive makes us check at runtime whether the value fits
impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(TaskId::for_index_and_gen(
        SYSCON as usize,
        Generation::default(),
    ));

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    muck_with_gpios(&syscon);

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual SPI block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM8::ptr() };

    let registers = unsafe { &*device::SPI8::ptr() };

    let mut spi = spi_core::Spi::from(registers);

    // Set SPI mode for Flexcomm
    flexcomm.pselid.write(|w| w.persel().spi());

    // This should correspond to SPI mode 0
    spi.initialize(
        device::spi0::cfg::MASTER_A::SLAVE_MODE,
        device::spi0::cfg::LSBF_A::STANDARD, // MSB First
        device::spi0::cfg::CPHA_A::CHANGE,
        device::spi0::cfg::CPOL_A::LOW,
        spi_core::TxLvl::TxEmpty,
        spi_core::RxLvl::Rx1Item,
    );

    spi.enable();

    // Field messages.
    let mask = 1;

    sys_irq_control(1, true);

    loop {

        let m = sys_recv_open(&mut [], mask);


        if m.sender == TaskId::KERNEL {
            // We recevied an interrupt without a caller. We need to send
            // something back

            if spi.cs_asserted() && !spi.cs_deasserted() {
                while !spi.cs_deasserted() {

                    spi.send_u8(0xff);
                    let _ = spi.read_u8();
                }
            }

            spi.clear_cs_state();

        } else {
            let caller = userlib::hl::Caller::from(m.sender);

            if m.lease_count != 2 {
                caller.reply_fail(ResponseCode::BadArg);
                continue;
            }

            let borrow_send = caller.borrow(0);
            let send_info = match borrow_send.info() {
                Some(s) => s,
                None => {
                    caller.reply_fail(ResponseCode::BadArg);
                    continue;
                }
            };

            
            if !send_info.attributes.contains(LeaseAttributes::READ) {
                caller.reply_fail(ResponseCode::BadArg);
                continue
            }

            let borrow_recv = caller.borrow(1);
            let recv_info = match borrow_recv.info() {
                Some(s) => s,
                None => { 
                     caller.reply_fail(ResponseCode::BadArg);
                     continue;
                }
            };

            if !recv_info.attributes.contains(LeaseAttributes::WRITE) {
                caller.reply_fail(ResponseCode::BadArg);
                continue
            }

            if recv_info.len != send_info.len {
                caller.reply_fail(ResponseCode::BadArg);
                continue
            }

            let mut s = SpiState {
                task : caller,
                tx_pos : 0,
                tx_lease_num: 0,
                rx_pos: 0,
                rx_lease_num: 1,
                len: recv_info.len,
            };


            let mut ret : Result<(), ResponseCode> = Ok(());
           

            cortex_m_semihosting::hprintln!("uh {} {}", spi.cs_asserted(), spi.cs_deasserted());

            if spi.cs_asserted() {
                spi.enable_tx();
                spi.enable_rx();

                while !spi.cs_deasserted() {
                    cortex_m_semihosting::hprintln!("c");
                    if spi.txerr() || spi.rxerr() {
                        ret = Err(ResponseCode::Underrun);
                        spi.clear_fifo_err();
                        break;
                    }

                    if spi.can_tx() {
                        ret = tx_byte(&mut spi, &mut s);
                        if ret.is_err() {
                            break;
                        }
                    }

                    if spi.has_byte() {
                        ret = rx_byte(&mut spi, &mut s);
                        if ret.is_err() {
                            break;
                        }
                    }
                

                    sys_recv_closed(&mut [], mask, TaskId::KERNEL)
                        .expect("notification died");
                }
    
                spi.disable_tx();
                spi.disable_rx();
                spi.clear_cs_state();
                cortex_m_semihosting::hprintln!("wat {} {}", spi.cs_asserted(), spi.cs_deasserted());
            }
            // XXX Need to get number of bytes
            cortex_m_semihosting::hprintln!("c {:x?}", ret);
            s.task.reply_result(ret);
        }
    }

}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi);
    syscon.leave_reset(Peripheral::HsLspi);

    syscon.enable_clock(Peripheral::Fc3);
    syscon.leave_reset(Peripheral::Fc3);
}

fn muck_with_gpios(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    // This is quite the array!
    // HS_SPI_MOSI = P0_26 = FUN9
    // HS_SPI_MISO = P1_3 = FUN6
    // HS_SPI_SCK = P1_2 = FUN6
    // HS_SPI_SSEL0 = P0_20 = FUN8
    // HS_SPI_SSEL1 = P1_1 = FUN5
    // HS_SPI_SSEL2 = P1_12 = FUN5
    // HS_SPI_SSEL3 = P1_26 = FUN5
    //
    // Some of the alt functions aren't defined in the HAL crate
    //
    // All of these need to be in digital mode. The NXP C driver
    // also sets the pull-up resistor
    let iocon = unsafe { &*device::IOCON::ptr() };

    iocon.pio0_26.write(|w| unsafe {
        w.func().bits(0x9).digimode().digital().mode().pull_up()
    });
    iocon
        .pio1_3
        .write(|w| w.func().alt6().digimode().digital().mode().pull_up());
    iocon.pio1_2.write(|w| w.func().alt6().digimode().digital());
    iocon.pio0_20.write(|w| unsafe {
        w.func().bits(0x8).digimode().digital().mode().pull_up()
    });
    iocon
        .pio1_1
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
    iocon
        .pio1_12
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
    iocon
        .pio1_26
        .write(|w| w.func().alt5().digimode().digital().mode().pull_up());
}


fn tx_byte(spi: &mut spi_core::Spi, s: &mut SpiState) -> Result<(), ResponseCode> {

    if s.tx_pos == s.len {
        spi.send_u8(0xff);
        // Is transmitting more an error?
        return Ok(());
    }

    let byte : u8 = s.task.borrow(s.tx_lease_num).read_at::<u8>(s.tx_pos).ok_or(ResponseCode::BadArg)?;
    spi.send_u8(byte);
    s.tx_pos += 1;
    Ok(())
}

fn rx_byte(spi: &mut spi_core::Spi, s: &mut SpiState) -> Result<(), ResponseCode> {
    // We received something but no room, just drop it?
    if s.rx_pos == s.len {
        return Ok(())
    }

    let byte = spi.read_u8();
    s.task.borrow(s.rx_lease_num).write_at(s.rx_pos, byte).ok_or(ResponseCode::BadArg)?;
    s.rx_pos += 1;
    Ok(())
}
