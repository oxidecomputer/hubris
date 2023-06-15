// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_stm32xx_i2c::*;
use drv_stm32xx_sys_api::{Mode, OutputType, PinSet, Pull, Speed, Sys};

use fixedmap::*;
use ringbuf::*;
use userlib::*;

task_slot!(SYS, sys);

fn lookup_controller<'a, 'b>(
    controllers: &'a [I2cController<'b>],
    controller: Controller,
) -> Result<&'a I2cController<'b>, ResponseCode> {
    controllers
        .iter()
        .find(|c| c.controller == controller)
        .ok_or(ResponseCode::BadController)
}

///
/// Validates a port for the specified controller.
///
fn validate_port(
    pins: &[I2cPins],
    controller: Controller,
    port: PortIndex,
) -> Result<(), ResponseCode> {
    pins.iter()
        .find(|pin| pin.controller == controller && pin.port == port)
        .ok_or(ResponseCode::BadPort)?;

    Ok(())
}

fn find_mux(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    id: Mux,
    mut func: impl FnMut(&I2cMux<'_>) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    for mux in muxes {
        if mux.controller != controller.controller {
            continue;
        }

        if mux.port != port || mux.id != id {
            continue;
        }

        return func(mux);
    }

    Err(ResponseCode::MuxNotFound)
}

fn all_muxes(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    mut func: impl FnMut(&I2cMux<'_>) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    for mux in muxes {
        if mux.controller != controller.controller {
            continue;
        }

        if mux.port != port {
            continue;
        }

        func(mux)?;
    }

    Ok(())
}

fn configure_mux(
    muxmap: &mut MuxMap,
    controller: &I2cController<'_>,
    port: PortIndex,
    mux: Option<(Mux, Segment)>,
    muxes: &[I2cMux<'_>],
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    //
    // XXX If we aren't doing an operation to a segment on a mux and we have had a
    // mux+segment enabled on this bus, we explicitly disable all segments on
    // the formerly enabled mux.  On the one hand, this shouldn't be strictly
    // necessarily (we generally design I2C addresses to avoid conflicts with
    // enabled segments), but on the other, we want to minimize the ability of
    // a bad component on a mux'd segment (e.g., a FRU) to wreak havoc
    // elsewhere in the system -- especially because the failure mode of an
    // (errant) address conflict can be pretty brutal.
    //
    let muxkey = (controller.controller, port);

    match muxmap.get(muxkey) {
        Some(MuxState::Selected(current_id, current_segment)) => {
            match mux {
                Some((id, segment)) if id == current_id => {
                    if segment == current_segment {
                        return Ok(());
                    }
                }
                _ => {
                    find_mux(controller, port, muxes, current_id, |mux| {
                        mux.driver.enable_segment(mux, controller, None, ctrl)
                    }).map_err(|err| {
                        muxmap.insert(muxkey, MuxState::Unknown);
                        err
                    })?;

                    muxmap.remove(muxkey);
                }
            }
        }

        Some(MuxState::Unknown) => {
            all_muxes(controller, port, muxes, |mux| {
                mux.driver.enable_segment(mux, controller, None, ctrl)
            })?;
            muxmap.remove(muxkey);
        }

        None => {}
    }

    if let Some((id, segment)) = mux { 
        find_mux(controller, port, muxes, id, |mux| {
            mux.driver.enable_segment(mux, controller, Some(segment), ctrl)
                .map_err(|err| {
                    muxmap.insert(muxkey, MuxState::Unknown);
                    err
                })?;

            muxmap.insert(muxkey, MuxState::Selected(id, segment));
            Ok(())
        })?;
    }

    Ok(())
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Error(u8, ResponseCode),
    MuxError(ResponseCode),
    Reset(Controller, PortIndex),
    MuxUnknown(Controller, PortIndex),
    ResetMux(Mux),
    SegmentFailed(ResponseCode),
    ConfigureFailed(ResponseCode),
    Wiggles(u8),
    None,
}

ringbuf!(Trace, 128, Trace::None);

fn reset(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    mux: Option<(Mux, Segment)>,
) {
    ringbuf_entry!(Trace::Reset(controller.controller, port));

/*
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);
*/

    // First, bounce our I2C controller
    controller.reset();

/*
    // And now reset the mux, eating any errors.
    let _ = find_mux(controller, port, muxes, mux, |mux, id, _| {
        ringbuf_entry!(Trace::ResetMux(id));
        mux.driver.reset(mux, &sys)?;
        Ok(())
    });
*/
}

fn reset_needed(code: ResponseCode) -> bool {
    match code {
        ResponseCode::BusLocked
        | ResponseCode::BusLockedMux
        | ResponseCode::BusReset
        | ResponseCode::BusResetMux
        | ResponseCode::BusError
        | ResponseCode::ControllerBusy => true,
        _ => false,
    }
}

fn reset_if_needed(
    code: ResponseCode,
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    mux: Option<(Mux, Segment)>,
) {
    if reset_needed(code) {
        reset(controller, port, muxes, mux)
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

type PortMap = FixedMap<Controller, PortIndex, { i2c_config::NCONTROLLERS }>;

#[derive(Copy, Clone, Debug)]
enum MuxState {
    Selected(Mux, Segment),
    Unknown,
}

type MuxMap = FixedMap<
    (Controller, PortIndex),
    MuxState,
    { i2c_config::NMUXEDBUSES },
>;

#[export_name = "main"]
fn main() -> ! {
    let controllers = i2c_config::controllers();
    let pins = i2c_config::pins();
    let muxes = i2c_config::muxes();

    // This is our actual mutable state
    let mut portmap = PortMap::default();
    let mut muxmap = MuxMap::default();

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c(&controllers);
    configure_pins(&controllers, &pins, &mut portmap);
    configure_controllers(&controllers);

    // Field messages.
    let mut buffer = [0; 4];

    let ctrl = I2cControl {
        enable: |notification| {
            sys_irq_control(notification, true);
        },
        wfi: |notification| {
            let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
        },
    };

    configure_muxes(&muxes, &controllers, &pins, &mut portmap, &mut muxmap, &ctrl);

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead | Op::WriteReadBlock => {
                let lease_count = msg.lease_count();

                let (payload, caller) = msg
                    .fixed::<[u8; 4], usize>()
                    .ok_or(ResponseCode::BadArg)?;

                if lease_count < 2 || lease_count % 2 != 0 {
                    return Err(ResponseCode::IllegalLeaseCount);
                }

                let (addr, controller, port, mux) =
                    Marshal::unmarshal(payload)?;

                if ReservedAddress::from_u8(addr).is_some() {
                    return Err(ResponseCode::ReservedAddress);
                }

                let controller = lookup_controller(&controllers, controller)?;
                validate_port(&pins, controller.controller, port)?;

                configure_port(&mut portmap, controller, port, &pins);

                match configure_mux(
                    &mut muxmap,
                    controller,
                    port,
                    mux,
                    &muxes,
                    &ctrl,
                ) {
                    Ok(_) => {}
                    Err(code) => {
                        ringbuf_entry!(Trace::MuxError(code));
                        reset_if_needed(code, controller, port, &muxes, mux);
                        return Err(code);
                    }
                }

                let mut total = 0;

                //
                // Now iterate over our write/read pairs (we have already
                // verified that we have an even number of leases).
                //
                for i in (0..lease_count).step_by(2) {
                    let wbuf = caller.borrow(i);
                    let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;

                    if !winfo.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    let rbuf = caller.borrow(i + 1);
                    let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;

                    if winfo.len == 0 && rinfo.len == 0 {
                        // In a given lease pair, we must have either a write
                        // OR a read -- while perhaps valid to support both
                        // being zero as a way of testing an address for a
                        // NACK, it's not a mode that we (currently) support.
                        return Err(ResponseCode::BadArg);
                    }

                    if winfo.len > 255 || rinfo.len > 255 {
                        // For now, we don't support writing or reading more
                        // than 255 bytes.
                        return Err(ResponseCode::BadArg);
                    }

                    let mut nread = 0;

                    match controller.write_read(
                        addr,
                        winfo.len,
                        |pos| wbuf.read_at(pos),
                        // Only the final read operation in a WriteReadBlock is
                        // a block read; everything else is a normal read.
                        if op == Op::WriteReadBlock && i == lease_count - 2 {
                            ReadLength::Variable
                        } else {
                            ReadLength::Fixed(rinfo.len)
                        },
                        |pos, byte| {
                            if pos + 1 > nread {
                                nread = pos + 1;
                            }

                            rbuf.write_at(pos, byte)
                        },
                        &ctrl,
                    ) {
                        Err(code) => {
                            ringbuf_entry!(Trace::Error(addr, code));
                            reset_if_needed(
                                code, controller, port, &muxes, mux,
                            );
                            return Err(code);
                        }
                        Ok(_) => {
                            total += nread;
                        }
                    }
                }

                caller.reply(total);
                Ok(())
            }
            Op::SelectedMuxSegment => {
                let (payload, caller) = msg
                    .fixed::<[u8; 4], [u8; 4]>()
                    .ok_or(ResponseCode::BadArg)?;

                let (address, controller, port, _) =
                    Marshal::unmarshal(payload)?;

                let controller = lookup_controller(&controllers, controller)?;
                validate_port(&pins, controller.controller, port)?;

                caller.reply(Marshal::marshal(&(
                    address,
                    controller.controller,
                    port,
                    None,
// XXXX
// muxmap.get((controller.controller, port)),
                )));

                Ok(())
            }
        });
    }
}

fn turn_on_i2c(controllers: &[I2cController<'_>]) {
    let sys = Sys::from(SYS.get_task_id());

    for controller in controllers {
        controller.enable(&sys);
    }
}

fn configure_controllers(controllers: &[I2cController<'_>]) {
    for controller in controllers {
        controller.configure();
        sys_irq_control(controller.notification, true);
    }
}

fn configure_port(
    map: &mut PortMap,
    controller: &I2cController<'_>,
    port: PortIndex,
    pins: &[I2cPins],
) {
    let current = map.get(controller.controller).unwrap();

    if current == port {
        return;
    }

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    //
    // We will now iterate over all pins, de-configuring any that match our
    // old port, and configuring any that match our new port.
    //
    for pin in pins
        .iter()
        .filter(|p| p.controller == controller.controller)
    {
        if pin.port == current {
            //
            // We de-configure our current port by setting the pins to
            // `Mode::Analog`, which tristates them and disables the input
            // buffer so the I2C peripheral doesn't respond to their state.
            //
            // At the same time, we leave the alternate function mux set to the
            // I2C device to prevent glitching when we turn the port back on.
            //
            // This is a slightly unusual operation that lacks a convenience
            // operation in the GPIO API, so we do it longhand:
            //
            for gpio_pin in &[pin.scl, pin.sda] {
                sys.gpio_configure(
                    gpio_pin.port,
                    gpio_pin.pin_mask,
                    Mode::Analog,
                    OutputType::OpenDrain,
                    Speed::Low,
                    Pull::None,
                    pin.function,
                );
            }
        } else if pin.port == port {
            for gpio_pin in &[pin.scl, pin.sda] {
                // Configure our new port!
                sys.gpio_configure_alternate(
                    *gpio_pin,
                    OutputType::OpenDrain,
                    Speed::Low,
                    Pull::None,
                    pin.function,
                );
            }
        }
    }

    map.insert(controller.controller, port);
}

///
/// When the system is reset without power loss, I2C can be in an arbitrary
/// state with respect to the bus -- and we can therefore come to life with a
/// transaction already in flight.  It is very important that we abort any
/// such transaction:  failure to do so will result in our first I2C
/// transaction being corrupted.  (And especially because our first I2C
/// transactions may well be to disable segments on a mux, this can result in
/// nearly arbitrary mayhem down the road!)  To do this, we engage in the
/// time-honored[0] tradition of "clocking through the problem":  wiggling SCL
/// until we see SDA high, and then pulling SDA low and releasing SCL to
/// indicate a STOP condition.  (Note that we need to do this up to 9 times to
/// assure that we have clocked through the entire transaction.)
///
/// [0] Analog Devices. AN-686: Implementing an I2C Reset. 2003.
///
fn wiggle_scl(sys: &Sys, scl: PinSet, sda: PinSet) {
    sys.gpio_configure_input(sda, Pull::None);
    sys.gpio_set(scl);

    sys.gpio_configure_output(
        scl,
        OutputType::OpenDrain,
        Speed::Low,
        Pull::None,
    );

    for i in 0..9 {
        if sys.gpio_read(sda) != 0 {
            //
            // SDA is high. We're going to flip it to an output, pull the
            // clock down then, pull SDA down, then release SCL and finally
            // release SDA.  This will denote a STOP condition.
            //
            sys.gpio_set(sda);

            sys.gpio_configure_output(
                sda,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::None,
            );

            sys.gpio_reset(scl);
            sys.gpio_reset(sda);
            sys.gpio_set(scl);
            sys.gpio_set(sda);
            ringbuf_entry!(Trace::Wiggles(i));
            break;
        }

        //
        // SDA is low -- someone is holding it down: give SCL a wiggle to try
        // to shake them.  Note that we don't sleep here:  we are relying on
        // the fact that communicating to the GPIO task is going to take
        // longer than our minimum SCL pulse.  (Which, on a 400 MHz H753, is
        // on the order of ~15 usecs -- yielding a cycle time of ~30 usecs
        // or ~33 KHz.)
        //
        sys.gpio_reset(scl);
        sys.gpio_set(scl);
    }
}

fn configure_pins(
    controllers: &[I2cController<'_>],
    pins: &[I2cPins],
    map: &mut PortMap,
) {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    //
    // Before we configure our pins, wiggle SCL to shake off any old
    // transaction.
    //
    for pin in pins {
        wiggle_scl(&sys, pin.scl, pin.sda);
    }

    for pin in pins {
        let controller =
            lookup_controller(controllers, pin.controller).ok().unwrap();

        match map.get(controller.controller) {
            Some(port) if port != pin.port => {
                //
                // If we have already enabled this controller with a different
                // port, we want to set this pin to its unselected state to
                // prevent glitches when we first use it.
                //
                for gpio_pin in &[pin.scl, pin.sda] {
                    sys.gpio_configure(
                        gpio_pin.port,
                        gpio_pin.pin_mask,
                        Mode::Analog,
                        OutputType::OpenDrain,
                        Speed::Low,
                        Pull::None,
                        pin.function,
                    );
                }

                continue;
            }
            _ => {}
        }

        for gpio_pin in &[pin.scl, pin.sda] {
            sys.gpio_configure_alternate(
                *gpio_pin,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::None,
                pin.function,
            );
        }

        map.insert(controller.controller, pin.port);
    }
}

fn configure_muxes(
    muxes: &[I2cMux<'_>],
    controllers: &[I2cController<'_>],
    pins: &[I2cPins],
    map: &mut PortMap,
    muxmap: &mut MuxMap,
    ctrl: &I2cControl,
) {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for mux in muxes {
        let controller =
            lookup_controller(controllers, mux.controller).unwrap();
        configure_port(map, controller, mux.port, pins);

        let mut reset_attempted = false;

        loop {
            match mux.driver.configure(mux, controller, &sys, ctrl) {
                Ok(_) => {
                    //
                    // We are going to attempt to disable all segments.  If we
                    // get an error here and that error indicates that we need
                    // to reset the controller, we will do so, but only once:
                    // if the mux has segments that are in a different power
                    // domain, it is conceivable that we will get what appears
                    // to be hung bus that will not be resolved by us
                    // resetting the controller -- and we don't want to spin
                    // forever here.
                    //
                    // In terms of why we might see a resolvable reset: we
                    // have noticed an issue whereby the first I2C transaction
                    // on some busses (notably, those that share controllers
                    // via pin muxing) will result in SCL being spuriously
                    // held down (see #1034 for details).  Resets of the I2C
                    // controller seem to always resolve the issue, so we want
                    // to do that reset now if we see a condition that
                    // indicates it:  we don't want to allow it to lie in wait
                    // for the first I2C transaction (which may or may not
                    // deal with the reset).
                    //
                    if let Err(code) =
                        mux.driver.enable_segment(mux, controller, None, ctrl)
                    {
                        ringbuf_entry!(Trace::SegmentFailed(code));

                        if reset_needed(code) && !reset_attempted {
                            reset(controller, mux.port, muxes, None);
                            reset_attempted = true;
                            continue;
                        }

                        let muxkey = (controller.controller, mux.port);
                        ringbuf_entry!(Trace::MuxUnknown(muxkey.0, muxkey.1));
                        muxmap.insert(muxkey, MuxState::Unknown);
                    }

                    break;
                }
                Err(code) => {
                    ringbuf_entry!(Trace::ConfigureFailed(code));
                    reset_if_needed(code, controller, mux.port, muxes, None);
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
