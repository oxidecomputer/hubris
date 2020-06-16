//! A driver for the LPC55 AES block
//!
//! Currently just supports AES-ECB. Should certainly support more later.
//!
//! This hardware block really wants to be interrupt driven and seems to
//! stall if you try and use less than two blocks. It's also easy to get
//! out of sequence if you forget to read the output data. Some of this
//! oddness may be related to timing. It seems like some of the engines
//! may have timing requirements we need to wait for.
//!
//! There also seems to be something odd with the endianness (see also
//! setting numerous bits to endian swap and the call in getting the
//! data) which may just be related to the choice of data...
//!
//! # IPC protocol
//!
//! ## `encrypt` (1)
//!
//! Encrypts the contents of lease #0 (R) to lease #1 (RW) using key (arg #0)
//! Only supports AES-ECB mode

#![no_std]
#![no_main]

use lpc55_pac as device;
use zerocopy::AsBytes;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

#[derive(Copy, Clone, Debug, FromPrimitive)]
enum Operation {
    Encrypt = 1
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

struct CryptData {
    caller: hl::Caller<()>,
    len: usize,
    rpos: usize,
    wpos: usize,
    need_load: bool,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_aes();

    let aes = unsafe { &*device::HASHCRYPT::ptr() };

    // This hardware block is a bit quirky so don't set up anything
    // beforehand
    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut c: Option<CryptData> = None;

    let mut buffer = [0; 16];
    loop {
        //let msginfo = sys_recv(key.as_bytes_mut(), mask);
        hl::recv(
            &mut buffer,
            mask,
            &mut c,
            |cryptref, bits| {

                if bits & 1 != 0 {
                    // This block expects all data to be loaded before we read
                    // out the digest/encrypted data
                    if aes.status.read().digest().bit() {
                        get_data(&aes, cryptref)
                    } else if aes.status.read().waiting().bit() {
                        // Shove more data to the block
                        load_a_block(&aes, cryptref)
                    } else if aes.status.read().error().bit() {
                        cortex_m_semihosting::hprintln!("AES error").ok();
                    }
                    sys_irq_control(1, true);
                }
            },
            |cryptref, op, msg| match op {
                Operation::Encrypt => {
                    let (&key, caller) = msg.fixed_with_leases::<[u32; 4], ()>(2)
                                        .ok_or(ResponseCode::BadArg)?;

                    if cryptref.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let src = caller.borrow(0);
                    let src_info = src.info().ok_or(ResponseCode::BadArg)?;

                    if !src_info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    let dst = caller.borrow(1);
                    let dst_info = dst.info().ok_or(ResponseCode::BadArg)?;

                    if !dst_info.attributes.contains(LeaseAttributes::WRITE) {
                        return Err(ResponseCode::BadArg);
                    }

                    if src_info.len != dst_info.len {
                        return Err(ResponseCode::BadArg);
                    }

                    // Yes we set new hash twice. Based on the reference
                    // driver we need to set the new has before we switch
                    // modes to ensure this gets picked up corectly
                    aes.ctrl.modify(|_, w| w.new_hash().start());
                    aes.ctrl.modify(|_, w| w.new_hash().start()
                                    .mode().aes()
                                    .hashswpb().set_bit()
                                    );

                    // Just use AES-ECB for now. We do need to do the
                    // endian swap as well
                    aes.cryptcfg.modify(|_, w|
                         w.aesmode().ecb()
                            .aesdecrypt().encrypt()
                            .aessecret().normal_way()
                            .aeskeysz().bits_128()
                            .msw1st_out().set_bit()
                            .msw1st().set_bit()
                            .swapkey().set_bit()
                            .swapdat().set_bit()
                    );

                    // The hardware supports 128-bit, 192-bit, and 256-bit
                    // keys but we only support 128-bit for now
            
                    unsafe {
                        aes.indata.write( |w| w.data().bits(key[0]) );
                        aes.indata.write( |w| w.data().bits(key[1]) );
                        aes.indata.write( |w| w.data().bits(key[2]) );
                        aes.indata.write( |w| w.data().bits(key[3]) );
                    }

                    // wait for the key to be loaded. We could potentially
                    // loop forever if we haven't set up the key correctly
                    // but looping forever is actually better behavior than
                    // potentially interrupting forever with the AES block.
                    //
                    // NEEDKEY also sets the waiting interrupt so maybe
                    // it would be cleaner just to do that on the first
                    // interrupt?
                    while aes.status.read().needkey().bit() { }

                    *cryptref = Some(CryptData {
                        caller,
                        rpos: 0,
                        wpos: 0,
                        len: dst_info.len,
                        need_load: true,
                    });

                    aes.intenset.modify(|_, w| w.waiting().set_bit());
                    Ok(())
                },
            }

        );
    }
}


fn turn_on_aes() {
    let rcc_driver = TaskId::for_index_and_gen(SYSCON as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 82; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn load_a_block(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };


    if !cdata.need_load {
        return;
    }

    if let Some(data) = cdata.caller.borrow(0).read_at::<[u32; 4]>(cdata.rpos) {
        unsafe {
            aes.indata.write( |w| w.data().bits(data[0]) );
            aes.indata.write( |w| w.data().bits(data[1]) );
            aes.indata.write( |w| w.data().bits(data[2]) );
            aes.indata.write( |w| w.data().bits(data[3]) );
            cdata.rpos += 16
        }

        if cdata.rpos == cdata.len {
            // Turn off the interrupt for waiting for data
            //
            cdata.need_load = false;
            aes.intenclr.write(|w| w.waiting().set_bit());
            aes.intenset.modify(|_, w| w.digest().set_bit());
        }
    } else {
        core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
    }
}


fn get_data(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    let mut data : [u32; 4] = [0; 4];

    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };

    data[0] = u32::from_be(aes.digest0[0].read().digest().bits());
    data[1] = u32::from_be(aes.digest0[1].read().digest().bits());
    data[2] = u32::from_be(aes.digest0[2].read().digest().bits());
    data[3] = u32::from_be(aes.digest0[3].read().digest().bits());

    if let Some(()) = cdata.caller.borrow(1).write_at::<[u32; 4]>(cdata.wpos, data) {
        cdata.wpos += 16;
        if cdata.wpos == cdata.len {
            aes.intenclr.write(|w| w.digest().set_bit().
                                    waiting().set_bit());

            core::mem::replace(c, None).unwrap().caller.reply(());
        }
    } else {
        core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
    }
}
