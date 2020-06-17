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
//!
//! ## `SHA1` (2)
//!
//! Performs the SHA-1 operation on lease #1 (R) and stores the result in lease
//! #1 (RW)
//!
//! ## `SHA256` (3)
//!
//! Performs the SHA-256 operation on lease #1 (R) and stores the result in
//! lease #1 (RW)

#![no_std]
#![no_main]

use lpc55_pac as device;
use zerocopy::AsBytes;
use userlib::*;
use core::convert::TryInto;
use core::cmp::max;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
enum Operation {
    Encrypt = 1,
    SHA1 = 2,
    SHA256 = 3,
}

#[repr(u32)]
enum ResponseCode {
    Success = 1,
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
    op: Operation,
    len: usize,
    rpos: usize,
    wpos: usize,
    need_load: bool,
    bcount: usize,
    total_blocks: usize,
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
                    if aes.status.read().waiting().bit() {
                        // Shove more data to the block
                        load_a_block(&aes, cryptref)
                    } 
                    if aes.status.read().digest().bit() {
                        get_data(&aes, cryptref)
                    } 
                    if aes.status.read().error().bit() {
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
                        cortex_m_semihosting::hprintln!("a").ok();
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

                    if dst_info.len != src_info.len {
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
                        op, 
                        rpos: 0,
                        wpos: 0,
                        len: dst_info.len,
                        need_load: true,
                        bcount: 0,
                        total_blocks: 0
                    });

                    aes.intenset.modify(|_, w| w.waiting().set_bit());
                    Ok(())
                },

                Operation::SHA1 => {
                    cortex_m_semihosting::hprintln!("ding").ok();
                    let (_, caller) = msg.fixed_with_leases::<[u32; 4], ()>(2)
                                        .ok_or(ResponseCode::BadArg)?;

                    cortex_m_semihosting::hprintln!("ding").ok();
                    if cryptref.is_some() {
                        cortex_m_semihosting::hprintln!("a").ok();
                        return Err(ResponseCode::Busy);
                    }

                    let src = caller.borrow(0);
                    let src_info = src.info().ok_or(ResponseCode::BadArg)?;

                    if !src_info.attributes.contains(LeaseAttributes::READ) {
                        cortex_m_semihosting::hprintln!("b").ok();
                        return Err(ResponseCode::BadArg);
                    }

                    let dst = caller.borrow(1);
                    let dst_info = dst.info().ok_or(ResponseCode::BadArg)?;

                    if !dst_info.attributes.contains(LeaseAttributes::WRITE) {
                        cortex_m_semihosting::hprintln!("c").ok();
                        return Err(ResponseCode::BadArg);
                    }

                    if dst_info.len != 4*5 {
                        cortex_m_semihosting::hprintln!("d").ok();
                        return Err(ResponseCode::BadArg);
                    }

                    // Yes we set new hash twice. Based on the reference
                    // driver we need to set the new has before we switch
                    // modes to ensure this gets picked up corectly
                    aes.ctrl.modify(|_, w| w.new_hash().start());
                    aes.ctrl.modify(|_, w| w.new_hash().start()
                                    .mode().sha1()
                                    .hashswpb().set_bit()
                                    );

                    // The engine works on 512-bit (16 word, 64 byte) chunks.
                    // This is the count of the number of whole blocks
                    let bcount = src_info.len / 64;

                    *cryptref = Some(CryptData {
                        caller,
                        op, 
                        rpos: 0,
                        wpos: 0,
                        bcount: 0,
                        len: src_info.len,
                        need_load: true,
                        total_blocks: bcount, 
                    });

                    aes.intenset.modify(|_, w| w.waiting().set_bit());
                    Ok(())
                }
                Operation::SHA256 => {
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

fn load_aes_block(aes: &device::hashcrypt::RegisterBlock, cdata: &mut CryptData) -> Option<ResponseCode>
{
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
            return Some(ResponseCode::Success);
        } else {
            return None;
        }
    } else {
        //core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
        return Some(ResponseCode::BadArg);
    }
}

// For blocks of < 512 bits
#[inline(never)]
fn load_partial_hash_block(aes: &device::hashcrypt::RegisterBlock, cdata: &mut CryptData) -> Option<ResponseCode>
{
    // This takes care of our zeroing so we don't have to pad later
    // We may end up with an unaligned message. This is much easier to
    // deal with as a u8 array than a u32 array
    let mut buf : [u8; 4*16] = [0; 4*16];
    let mut actual_final = true;
    // length of the whole message in bits
    // 16 words per block * 32 bits
    let mut len : u64 = (cdata.len*8) as u64;

    if cdata.rpos != cdata.len {
        let p : &mut [u8] = &mut buf;
        cortex_m_semihosting::hprintln!("before {}", actual_final).ok();
        if let Some(n) = cdata.caller.borrow(0).read_unfully_at(cdata.rpos, p) {
            cortex_m_semihosting::hprintln!("after {}", actual_final).ok();
            cdata.rpos += n;
            //cdata.bcount += 1;

            //cortex_m_semihosting::hprintln!("carp what {:x} {}", n, actual_final).ok();
            // last block needs to be less than 4*14
            if n >= 4*14 {
                //cortex_m_semihosting::hprintln!("not actually done");
                actual_final = false;
            }
            // This sets the final '1' but we can only do this if
            // there's space in this block. If we copied exactly 64 bytes
            // we need to defer the '1' to an empty block of all zeros
            if n != 4*16 {
                buf[n+1] = 0x80;
            }
            //cortex_m_semihosting::hprintln!("carp what {:x} {}", n, actual_final).ok();

            // 8-bits per word
            len += (n*8) as u64;
            // If we haven't 

        } else {
            return Some(ResponseCode::BadArg);
        }
    } else {
        // put the '1' at the start of the new block
        buf[0] = 0x80;
    }

    //cortex_m_semihosting::hprintln!("actually done {} {:x}", actual_final, len).ok();
    // XXX There has to be a better way to do this...
    if actual_final {
        buf[56] = ((len >> 0) & 0xff) as u8;
        buf[57] = ((len >> 8) & 0xff) as u8;
        buf[58] = ((len >> 16) & 0xff) as u8;
        buf[59] = ((len >> 24) & 0xff) as u8;
        buf[60] = ((len >> 32) & 0xff) as u8;
        buf[61] = ((len >> 40) & 0xff) as u8;
        buf[62] = ((len >> 48) & 0xff) as u8;
        buf[63] = ((len >> 54) & 0xff) as u8;
    }

    for i in 0..16 {
        unsafe {
            let mut v : u32= buf[i + 0] as u32;
            v |= ((buf[i + 1] as u32) << 8) as u32;
            v |= ((buf[i + 2] as u32) << 16) as u32;
            v |= ((buf[i + 3] as u32) << 24) as u32;
            //cortex_m_semihosting::hprintln!("[{}] {:0x}", i, v).ok();
            aes.indata.write( |w| w.data().bits(v) );
        }
    }
    
    //cortex_m_semihosting::hprintln!("actually done {}", actual_final).ok();
    if actual_final {
        //cortex_m_semihosting::hprintln!("done?").ok();
        cdata.need_load = false;
            
        aes.intenclr.write(|w| w.waiting().set_bit());
        aes.intenset.modify(|_, w| w.digest().set_bit());
        return Some(ResponseCode::Success);
        //return load_partial_hash_block(aes, cdata);
    } else {
        //cortex_m_semihosting::hprintln!("still going?").ok();
        return None;
    }
}

#[inline(never)]
fn load_full_hash_block(aes: &device::hashcrypt::RegisterBlock, cdata: &mut CryptData) -> Option<ResponseCode>
{
    cortex_m_semihosting::hprintln!("something {} {}", cdata.bcount, cdata.total_blocks).ok();
    if let Some(data) = cdata.caller.borrow(0).read_at::<[u32; 16]>(cdata.rpos) {
        unsafe {
            for i in 0..16 {
                cortex_m_semihosting::hprintln!("[{}] {:0x}", i, data[i]).ok();
                aes.indata.write( |w| w.data().bits(data[i]) );
            }
        }

        cdata.rpos += 64;
        cdata.bcount += 1;
        cortex_m_semihosting::hprintln!("something {} {}", cdata.bcount, cdata.total_blocks).ok();
        return None;
    } else {
        cortex_m_semihosting::hprintln!("Nooo??").ok();
        loop { }
        //core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
        return Some(ResponseCode::BadArg);
    }
}

#[inline(never)]
fn load_hash_block(aes: &device::hashcrypt::RegisterBlock, cdata: &mut CryptData) -> Option<ResponseCode>
{
    // the hashing works on 512-bit (16 word or 64 byte) blocks. Per the docs
    //
    // 1. The last data must be 447 bits or less. If more, then an extra block
    // must be created.
    //
    // 2. After the last bit of data, a ‘1’ bit is appended. Then, as many 0
    // bits are appended to take it to 448 bits long (so, 0 or more).
    //
    // 3. Finally, the last 64-bits contain the length of the whole message,
    // in bits, formatted as a word.
    //
    let remain = cdata.len - cdata.rpos;
    cortex_m_semihosting::hprintln!("aaaaa blurf {:x} {:x}", remain, 4*14);
    // Last block
    if remain < 4*16 {
        // Entire last block can be sent
        return load_partial_hash_block(aes, cdata);
    } else {
        return load_full_hash_block(aes, cdata);
    }


}

#[inline(never)]
fn load_a_block(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };


    if !cdata.need_load {
        return;
    }

    if cdata.op == Operation::Encrypt {
        load_aes_block(aes, cdata);
    } else {
        load_hash_block(aes, cdata);
    }
}

#[inline(never)]
fn get_hash_data(aes: &device::hashcrypt::RegisterBlock, c: &mut CryptData) -> Option<ResponseCode> {
    let mut data : [u32; 5] = [0; 5];

    if c.need_load {
        return None;
    }

    data[0] = u32::from_be(aes.digest0[0].read().digest().bits());
    data[1] = u32::from_be(aes.digest0[1].read().digest().bits());
    data[2] = u32::from_be(aes.digest0[2].read().digest().bits());
    data[3] = u32::from_be(aes.digest0[3].read().digest().bits());
    data[4] = u32::from_be(aes.digest0[4].read().digest().bits());

    if let Some(()) = c.caller.borrow(1).write_at::<[u32; 5]>(c.wpos, data) {
        aes.intenclr.write(|w| w.digest().set_bit().
                                    waiting().set_bit());
        //core::mem::replace(c, None).unwrap().caller.reply(());
        return Some(ResponseCode::Success);
    } else {
        //core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
        return Some(ResponseCode::BadArg);
    }
}

#[inline(never)]
fn get_aes_data(aes: &device::hashcrypt::RegisterBlock, c: &mut CryptData) -> Option<ResponseCode> {
    let mut data : [u32; 4] = [0; 4];

    data[0] = u32::from_be(aes.digest0[0].read().digest().bits());
    data[1] = u32::from_be(aes.digest0[1].read().digest().bits());
    data[2] = u32::from_be(aes.digest0[2].read().digest().bits());
    data[3] = u32::from_be(aes.digest0[3].read().digest().bits());

    if let Some(()) = c.caller.borrow(1).write_at::<[u32; 4]>(c.wpos, data) {
        c.wpos += 16;
        if c.wpos == c.len {
            aes.intenclr.write(|w| w.digest().set_bit().
                                    waiting().set_bit());

            //core::mem::replace(c, None).unwrap().caller.reply(());
            return Some(ResponseCode::Success);
        } else {
            return None
        }
    } else {
        //core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
        return Some(ResponseCode::BadArg);
    }
}

#[inline(never)]
fn get_data(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    //let mut data : [u32; 4] = [0; 4];

    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };

    cortex_m_semihosting::hprintln!("wat");
    let result = if cdata.op == Operation::Encrypt {
        get_aes_data(aes, cdata)
    } else {
        get_hash_data(aes, cdata)
    };

    match result {
        Some(s) => {
            match s {
                ResponseCode::Success => core::mem::replace(c, None).unwrap().caller.reply(()),
                _ => core::mem::replace(c, None).unwrap().caller.reply_fail(s)
            }
        }
        None => ()
    }
}
