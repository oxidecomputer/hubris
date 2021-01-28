use lpc55_pac as device;

pub fn turn_off_puf(
    puf: &device::puf::RegisterBlock,
    syscon: &device::syscon::RegisterBlock,
) {
    puf.pwrctrl.write(|w| w.ramon().clear_bit());

    // need to wait 400 ms
    // According to the programmed code we are at 48mhz
    // n ins = 400 ms *  48 000 000 ins / sec * 1 sec / 1000 msec
    //  = 192000000 instructions
    //
    //  This can probably be tuned
    cortex_m::asm::delay(192000000);

    syscon.presetctrl2.write(|w| w.puf_rst().set_bit());
    syscon.ahbclkctrl2.write(|w| w.puf().clear_bit());
}

pub fn turn_on_puf(
    puf: &device::puf::RegisterBlock,
    syscon: &device::syscon::RegisterBlock,
) {
    syscon.ahbclkctrl2.write(|w| w.puf().set_bit());

    // The NXP C driver explicitly puts this in reset so do this
    syscon.presetctrl2.write(|w| w.puf_rst().set_bit());
    syscon.presetctrl2.write(|w| w.puf_rst().clear_bit());

    puf.pwrctrl.write(|w| w.ramon().set_bit());

    while !puf.pwrctrl.read().ramstat().bit() {}
}

pub fn puf_init(
    puf: &device::puf::RegisterBlock,
    syscon: &device::syscon::RegisterBlock,
) -> Result<(), ()> {
    turn_on_puf(puf, syscon);
    puf_wait_for_init(puf)
}

fn puf_wait_for_init(puf: &device::puf::RegisterBlock) -> Result<(), ()> {
    while puf.stat.read().busy().bit() {}

    if puf.stat.read().success().bit() && !puf.stat.read().error().bit() {
        return Ok(());
    } else {
        return Err(());
    }
}

pub fn puf_enroll(
    puf: &device::puf::RegisterBlock,
    activation_code: &mut [u32; 298],
) -> Result<(), ()> {
    let mut idx = 0;

    if !puf.allow.read().allowenroll().bit() {
        return Err(());
    }

    // begin Enroll
    puf.ctrl.write(|w| w.enroll().set_bit());

    while !puf.stat.read().busy().bit() && !puf.stat.read().error().bit() {}

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeoutavail().bit() {
            let d = puf.codeoutput.read().bits();
            activation_code[idx] = d;
            idx += 1;
        }
    }

    if puf.stat.read().success().bit() {
        return Ok(());
    } else {
        return Err(());
    }
}

pub fn puf_start(
    puf: &device::puf::RegisterBlock,
    activation_code: &[u32; 298],
) -> Result<(), ()> {
    let mut idx = 0;

    while puf.stat.read().busy().bit() {}

    if !puf.allow.read().allowstart().bit() {
        return Err(());
    }

    puf.ctrl.write(|w| w.start().set_bit());

    while !puf.stat.read().busy().bit() && !puf.stat.read().error().bit() {}

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeinreq().bit() {
            puf.codeinput
                .write(|w| unsafe { w.codein().bits(activation_code[idx]) });
            idx += 1;
        }
    }

    if puf.stat.read().success().bit() {
        return Ok(());
    } else {
        return Err(());
    }
}

pub fn puf_get_key(
    puf: &device::puf::RegisterBlock,
    _key_index: u8,
    key_code: &[u32],
    key_data: &mut [u32],
) -> Result<(), ()> {
    let mut key_code_idx = 0;
    let mut key_data_idx = 0;

    if !puf.allow.read().allowgetkey().bit() {
        return Err(());
    }

    puf.ctrl.write(|w| w.getkey().set_bit());

    while !puf.stat.read().busy().bit() && !puf.stat.read().error().bit() {}

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeinreq().bit() {
            puf.codeinput
                .write(|w| unsafe { w.codein().bits(key_code[key_code_idx]) });
            key_code_idx += 1;
        }

        if puf.stat.read().keyoutavail().bit() {
            let _key_idx = puf.keyoutindex.read().bits();
            let d = puf.keyoutput.read().bits();
            key_data[key_data_idx] = d;
            key_data_idx += 1;
        }
    }

    if puf.stat.read().success().bit() {
        return Ok(());
    } else {
        return Err(());
    }
}

pub fn puf_set_intrinsic_key(
    puf: &device::puf::RegisterBlock,
    key_index: u8,
    key_bits: u32,
    key_code: &mut [u32],
) -> Result<(), ()> {
    let mut idx = 0;

    if !puf.allow.read().allowsetkey().bit() {
        return Err(());
    }

    // The NXP C driver gives this in bytes(?) but giving this in bits
    // is much more obvious. key_size gets written as bits % 64 in the register
    // per table 48.10.7.3
    if key_bits < 64 || key_bits > 4096 || key_bits % 64 != 0 {
        return Err(());
    }

    if key_index > 15 {
        return Err(());
    }

    puf.keysize
        .write(|w| unsafe { w.keysize().bits((key_bits >> 6) as u8) });
    puf.keyindex
        .write(|w| unsafe { w.keyidx().bits(key_index) });

    puf.ctrl.write(|w| w.generatekey().set_bit());

    while !puf.stat.read().busy().bit() && !puf.stat.read().error().bit() {}

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeoutavail().bit() {
            let out = puf.codeoutput.read().bits();
            key_code[idx] = out;
            idx += 1;
        }
    }

    if puf.stat.read().success().bit() {
        return Ok(());
    } else {
        return Err(());
    }
}
