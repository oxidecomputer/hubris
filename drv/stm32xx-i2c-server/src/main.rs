// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_stm32xx_i2c::*;
use drv_stm32xx_sys_api::{OutputType, Pull, Speed, Sys};

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
    pins: &[I2cPin],
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
    mux: Option<(Mux, Segment)>,
    mut func: impl FnMut(&I2cMux<'_>, Mux, Segment) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    match mux {
        Some((id, segment)) => {
            for mux in muxes {
                if mux.controller != controller.controller {
                    continue;
                }

                if mux.port != port || mux.id != id {
                    continue;
                }

                return func(mux, id, segment);
            }

            Err(ResponseCode::MuxNotFound)
        }
        None => Ok(()),
    }
}

fn configure_mux(
    map: &mut MuxMap,
    controller: &I2cController<'_>,
    port: PortIndex,
    mux: Option<(Mux, Segment)>,
    muxes: &[I2cMux<'_>],
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    //
    // If we aren't doing an operation to a segment on a mux and we have had a
    // mux+segment enabled on this bus, we explicitly disable all segments on
    // the formerly enabled mux.  On the one hand, this shouldn't be strictly
    // necessarily (we generally design I2C addresses o avoid conflicts with
    // enabled segments), but on the other, we want to minimize the ability of
    // a bad component on a mux'd segment (e.g., a FRU) to wreak havoc
    // elsewhere in the system -- especially because the failure mode of an
    // (errant) address conflict can be pretty brutal.
    //
    if mux.is_none() {
        if let Some(current) = map.get((controller.controller, port)) {
            find_mux(controller, port, muxes, Some(current), |old, _, _| {
                old.driver.enable_segment(old, controller, None, ctrl)
            })?;

            map.remove((controller.controller, port));
        }

        return Ok(());
    }

    find_mux(controller, port, muxes, mux, |mux, id, segment| {
        // Determine if the current segment matches our specified segment...
        if let Some(current) = map.get((controller.controller, port)) {
            if current.0 == id && current.1 == segment {
                return Ok(());
            }

            if current.0 != id {
                //
                // We are switching away from the old mux.  We need to find
                // it and disable all segments on it.
                //
                find_mux(
                    controller,
                    port,
                    muxes,
                    Some(current),
                    |old, _, _| {
                        old.driver.enable_segment(old, controller, None, ctrl)
                    },
                )?;
            }
        }

        //
        // If we're here, our mux is valid, but the current segment is not the
        // specified segment; we will now call upon our driver to enable this
        // segment.  Note that if we have an existing mux/segment, and we fail
        // to enable the new mux/segment, the map will not be updated.  This
        // is deliberate:  if we cannot enable a new mux/segment, it may very
        // well be because an errant device on the old segment is locking the
        // bus; if only for forensic purposes, we want to know what this mux +
        // segment was.
        //
        mux.driver
            .enable_segment(mux, controller, Some(segment), ctrl)?;
        map.insert((controller.controller, port), (id, segment));

        Ok(())
    })
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Error, // ringbuf line indicates error location
    Reset(Controller, PortIndex),
    ResetMux(Mux),
    SegmentFailed(ResponseCode),
    ConfigureFailed(ResponseCode),
    None,
}

ringbuf!(Trace, 8, Trace::None);

fn reset_if_needed(
    code: ResponseCode,
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    mux: Option<(Mux, Segment)>,
) {
    match code {
        ResponseCode::BusLocked
        | ResponseCode::BusLockedMux
        | ResponseCode::BusReset
        | ResponseCode::BusResetMux
        | ResponseCode::BusError
        | ResponseCode::ControllerLocked => {}
        _ => {
            return;
        }
    }

    ringbuf_entry!(Trace::Reset(controller.controller, port));

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    // First, bounce our I2C controller
    controller.reset();

    // And now reset the mux, eating any errors.
    let _ = find_mux(controller, port, muxes, mux, |mux, id, _| {
        ringbuf_entry!(Trace::ResetMux(id));
        mux.driver.reset(mux, &sys)?;
        Ok(())
    });
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

type PortMap = FixedMap<Controller, PortIndex, { i2c_config::NCONTROLLERS }>;

type MuxMap = FixedMap<
    (Controller, PortIndex),
    (Mux, Segment),
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

    configure_muxes(&muxes, &controllers, &pins, &mut portmap, &ctrl);

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead | Op::WriteReadBlock => {
                let (payload, caller) = msg
                    .fixed_with_leases::<[u8; 4], usize>(2)
                    .ok_or(ResponseCode::BadArg)?;

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
                        ringbuf_entry!(Trace::Error);
                        reset_if_needed(code, controller, port, &muxes, mux);
                        return Err(code);
                    }
                }

                let wbuf = caller.borrow(0);
                let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;

                if !winfo.attributes.contains(LeaseAttributes::READ) {
                    return Err(ResponseCode::BadArg);
                }

                let rbuf = caller.borrow(1);
                let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;

                if winfo.len == 0 && rinfo.len == 0 {
                    // We must have either a write OR a read -- while perhaps
                    // valid to support both being zero as a way of testing an
                    // address for a NACK, it's not a mode that we (currently)
                    // support.
                    return Err(ResponseCode::BadArg);
                }

                if winfo.len > 255 || rinfo.len > 255 {
                    // For now, we don't support writing or reading more than
                    // 255 bytes.
                    return Err(ResponseCode::BadArg);
                }

                let mut nread = 0;

                match controller.write_read(
                    addr,
                    winfo.len,
                    |pos| wbuf.read_at(pos),
                    if op == Op::WriteRead {
                        ReadLength::Fixed(rinfo.len)
                    } else {
                        ReadLength::Variable
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
                        ringbuf_entry!(Trace::Error);
                        reset_if_needed(code, controller, port, &muxes, mux);
                        Err(code)
                    }
                    Ok(_) => {
                        caller.reply(nread);
                        Ok(())
                    }
                }
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
                    muxmap.get((controller.controller, port)),
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
    pins: &[I2cPin],
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
            // `Mode::input`, which will assure that we don't leave SCL and
            // SDA pulled high.
            //
            sys.gpio_configure_input(pin.gpio_pins, Pull::None);
        } else if pin.port == port {
            // Configure our new port!
            sys.gpio_configure_alternate(
                pin.gpio_pins,
                OutputType::OpenDrain,
                Speed::High,
                Pull::None,
                pin.function,
            );
        }
    }

    map.insert(controller.controller, port);
}

fn configure_pins(
    controllers: &[I2cController<'_>],
    pins: &[I2cPin],
    map: &mut PortMap,
) {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for pin in pins {
        let controller =
            lookup_controller(controllers, pin.controller).ok().unwrap();

        match map.get(controller.controller) {
            Some(port) if port != pin.port => {
                //
                // If we have already enabled this controller with a different
                // port, we don't want to enable this pin.
                //
                continue;
            }
            _ => {}
        }

        sys.gpio_configure_alternate(
            pin.gpio_pins,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            pin.function,
        );

        map.insert(controller.controller, pin.port);
    }
}

fn configure_muxes(
    muxes: &[I2cMux<'_>],
    controllers: &[I2cController<'_>],
    pins: &[I2cPin],
    map: &mut PortMap,
    ctrl: &I2cControl,
) {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    for mux in muxes {
        let controller =
            lookup_controller(controllers, mux.controller).unwrap();
        configure_port(map, controller, mux.port, pins);

        loop {
            match mux.driver.configure(mux, controller, &sys, ctrl) {
                Ok(_) => {
                    //
                    // We are going to attempt to disable all segments -- but
                    // we are also going to need to eat the error here if
                    // we get one:  it's possible that the mux itself is
                    // powered off.
                    //
                    if let Err(code) =
                        mux.driver.enable_segment(mux, controller, None, ctrl)
                    {
                        ringbuf_entry!(Trace::SegmentFailed(code));
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
