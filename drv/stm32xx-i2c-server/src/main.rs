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

///
/// Calls `func` for the specified mux ID on the specified controller and
/// port -- or returns `ResponseCode::MuxNotFound` if there is no such mux
///
fn find_mux(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    id: Mux,
    mut func: impl FnMut(&I2cMux<'_>) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    for mux in muxes {
        if mux.controller == controller.controller
            && mux.port == port
            && mux.id == id
        {
            return func(mux);
        }
    }

    Err(ResponseCode::MuxNotFound)
}

///
/// Calls `func` for all muxes on the specified controller and port.
///
fn all_muxes(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    mut func: impl FnMut(&I2cMux<'_>) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    for mux in muxes {
        if mux.controller == controller.controller && mux.port == port {
            func(mux)?;
        }
    }

    Ok(())
}

///
/// Configure the mux+segment to use for the next transaction.  If anything
/// goes wrong here, the mux state will be set to unknown and an error
/// returned.  No operation should be performed on a bus without this
/// routine correctly returning!
///
fn configure_mux(
    muxmap: &mut MuxMap,
    controller: &I2cController<'_>,
    port: PortIndex,
    mux: Option<(Mux, Segment)>,
    muxes: &[I2cMux<'_>],
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    let bus = (controller.controller, port);

    match muxmap.get(bus) {
        Some(MuxState::Enabled(current_id, current_segment)) => match mux {
            Some((id, segment)) if id == current_id => {
                //
                // We have an enabled mux+segment on this bus, and it matches
                // our desired mux.  (If the segment matches, we're done and
                // can return; if the segment doesn't match we will set it
                // to our desired segment below.)
                //
                if segment == current_segment {
                    return Ok(());
                }
            }
            _ => {
                //
                // We have an enabled mux+segment on this bus, but it doesn't
                // match our desired mux -- which is to say that we have
                // either enabled a different mux or no mux at all.  In
                // either case, we will disable all segments on our currently
                // enabled mux.  If we are not enabling a mux at all, this
                // shouldn't be strictly necessarily (we generally design I2C
                // addresses to avoid conflicts with enabled segments), but we
                // want to minimize the ability of a bad component on a mux'd
                // segment (e.g., a FRU) to wreak havoc elsewhere in the
                // system -- especially because the failure mode of an
                // (errant) address conflict can be pretty brutal.
                //
                find_mux(controller, port, muxes, current_id, |mux| {
                    mux.driver.enable_segment(mux, controller, None, ctrl)
                })
                .map_err(|err| {
                    //
                    // We have failed to disable the segments on our current
                    // mux -- which means we are in an unknown mux state for
                    // this bus.  Set our state, and return the error.
                    //
                    muxmap.insert(bus, MuxState::Unknown);
                    err
                })?;

                //
                // We now know that no mux+segment is enabled; indicate
                // this by removing this bus from the muxmap.
                //
                muxmap.remove(bus);
            }
        },

        Some(MuxState::Unknown) => {
            //
            // We are in an unknown mux state.  Before we can do anything, we
            // need to successfully talk to every mux (or successfully learn
            // that the mux is gone entirely!), and disable every segment.  If
            // there is any failure through here that isn't the mux being
            // affirmatively gone, we'll just return the error, leaving our
            // mux state as unknown.
            //
            all_muxes(controller, port, muxes, |mux| {
                match mux.driver.enable_segment(mux, controller, None, ctrl) {
                    Err(ResponseCode::MuxMissing) => {
                        //
                        // The mux is gone entirely.  We really don't expect
                        // this on any production system, but it can be true on
                        // some special lab systems (you know who you are!).
                        // Regardless of its origin, we can limit the blast
                        // radius in this case: if the mux is affirmatively
                        // gone (that is, no device is acking its address), we
                        // can assume that the mux is absent rather than
                        // Byzantine -- and therefore assume that its segments
                        // are as good as disabled and allow other traffic on
                        // the bus.  So on this error (and only this error), we
                        // note that we saw it, and drive on.  (Note that
                        // attempting to speak to a device on a segment on the
                        // missing mux will properly return MuxMissing -- and
                        // set our bus's mux state to be unknown.)
                        //
                        ringbuf_entry!(Trace::MuxMissing(mux.address));
                        Ok(())
                    }
                    other => other,
                }
            })?;

            //
            // We have successfully transitioned to a known state -- namely,
            // that no mux+segment is enabled.  Indicate this by removing
            // this bus from the muxmap.
            //
            ringbuf_entry!(Trace::MuxUnknownRecover(bus));
            muxmap.remove(bus);
        }

        None => {}
    }

    //
    // We know that no mux+segment is enabled OR we have the current mux
    // but we need to enable a different segment.
    //
    if let Some((id, segment)) = mux {
        find_mux(controller, port, muxes, id, |mux| {
            mux.driver
                .enable_segment(mux, controller, Some(segment), ctrl)
                .map_err(|err| {
                    //
                    // We have failed to enable our new mux+segment.
                    // Transition ourselves into the unknown state and return
                    // the error.
                    //
                    muxmap.insert(bus, MuxState::Unknown);
                    err
                })?;

            //
            // We have succeeded, and we are in a known state with our
            // desired mux+segment correctly enabled.  Update our muxmap!
            //
            muxmap.insert(bus, MuxState::Enabled(id, segment));
            Ok(())
        })?;
    }

    Ok(())
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    SegmentOnError((Mux, Segment)),
    Error(u8, ResponseCodeU8),
    MuxError(ResponseCodeU8),
    Reset((Controller, PortIndex)),
    MuxUnknown((Controller, PortIndex)),
    MuxUnknownRecover((Controller, PortIndex)),
    MuxMissing(u8),
    ResetMux(u8),
    SegmentFailed(ResponseCodeU8),
    ConfigureFailed(ResponseCodeU8),
    Wiggles(u8),
}

ringbuf!(Trace, 160, Trace::None);

fn reset(
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    muxmap: &mut MuxMap,
) {
    let bus = (controller.controller, port);
    ringbuf_entry!(Trace::Reset(bus));

    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    // First, bounce our I2C controller
    controller.reset();

    // And now reset all muxes on this bus, eating any errors.
    let _ = all_muxes(controller, port, muxes, |mux| {
        ringbuf_entry!(Trace::ResetMux(mux.address));
        let _ = mux.driver.reset(mux, &sys);

        //
        // We now consider ourselves to be in an Unknown state:  it will
        // be up to the next transaction on this bus to properly set the
        // mux state.
        //
        muxmap.insert(bus, MuxState::Unknown);
        Ok(())
    });
}

fn reset_needed(code: ResponseCode) -> bool {
    matches!(
        code,
        ResponseCode::BusLocked
            | ResponseCode::BusLockedMux
            | ResponseCode::BusReset
            | ResponseCode::BusResetMux
            | ResponseCode::BusError
            | ResponseCode::ControllerBusy
    )
}

fn reset_if_needed(
    code: ResponseCode,
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    muxmap: &mut MuxMap,
) {
    if reset_needed(code) {
        reset(controller, port, muxes, muxmap)
    }
}

///
/// A variant of [`reset_if_needed`] that will also wiggle the SCL lines
/// via [`wiggle_scl`].
///
fn reset_and_wiggle_if_needed(
    code: ResponseCode,
    controller: &I2cController<'_>,
    port: PortIndex,
    muxes: &[I2cMux<'_>],
    muxmap: &mut MuxMap,
    pins: &[I2cPins],
) {
    if reset_needed(code) {
        let sys = SYS.get_task_id();
        let sys = Sys::from(sys);

        for pin in pins
            .iter()
            .filter(|p| p.controller == controller.controller)
            .filter(|p| p.port == port)
        {
            wiggle_scl(&sys, pin.scl, pin.sda);

            //
            // [`wiggle_scl`] puts our pins in output (and input) mode; set
            // them back to be configured for I2C before we reset.
            //
            for gpio_pin in &[pin.scl, pin.sda] {
                sys.gpio_configure_alternate(
                    *gpio_pin,
                    OutputType::OpenDrain,
                    Speed::Low,
                    Pull::None,
                    pin.function,
                );
            }
        }

        reset(controller, port, muxes, muxmap);
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

type PortMap = FixedMap<Controller, PortIndex, { i2c_config::NCONTROLLERS }>;

#[derive(Copy, Clone, Debug)]
enum MuxState {
    /// a mux+segment have been explicitly enabled
    Enabled(Mux, Segment),

    /// state is unknown: zero, one, or more mux+segment(s) may be enabled
    Unknown,
}

///
/// Contains the mux state on a per-bus basis.  If no mux+segment is enabled
/// for a bus (that is, if any/all muxes on a bus have been explicitly had
/// all segments disabled), there will not be an entry for the bus in this
/// map.
///
type MuxMap =
    FixedMap<(Controller, PortIndex), MuxState, { i2c_config::NMUXEDBUSES }>;

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
        wfi: |notification, timeout| {
            const TIMER_NOTIFICATION: u32 = 1 << 31;

            // If the driver passes in a timeout that is large enough that it
            // would overflow the kernel's 64-bit timestamp space... well, we'll
            // do the best we can without compiling in an unlikely panic.
            let dead = sys_get_timer().now.saturating_add(timeout.0);

            sys_set_timer(Some(dead), TIMER_NOTIFICATION);

            let notification =
                sys_recv_notification(notification | TIMER_NOTIFICATION);

            if notification == TIMER_NOTIFICATION {
                I2cControlResult::TimedOut
            } else {
                sys_set_timer(None, TIMER_NOTIFICATION);
                I2cControlResult::Interrupted
            }
        },
    };

    configure_muxes(
        &muxes,
        &controllers,
        &pins,
        &mut portmap,
        &mut muxmap,
        &ctrl,
    );

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
                        ringbuf_entry!(Trace::MuxError(code.into()));
                        reset_if_needed(
                            code,
                            controller,
                            port,
                            &muxes,
                            &mut muxmap,
                        );
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

                    let controller_result = controller.write_read(
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
                    );
                    match controller_result {
                        Err(code) => {
                            //
                            // NoDevice errors aren't hugely interesting --
                            // but on any other error, we want to record the
                            // address of the failing device, the error code
                            // and the mux+segment (if specified).
                            //
                            if code != ResponseCode::NoDevice {
                                ringbuf_entry!(Trace::Error(addr, code.into()));

                                if let Some(mux) = mux {
                                    ringbuf_entry!(Trace::SegmentOnError(mux));
                                }
                            }

                            reset_and_wiggle_if_needed(
                                code,
                                controller,
                                port,
                                &muxes,
                                &mut muxmap,
                                &pins,
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
/// When the system is either reset without power loss (e.g., due to an SP
/// upgrade) or I2C is preempted longer than the 25ms I2C timeout (e.g., due
/// to a large process panicking and being dumped by jefe), I2C can be in an
/// arbitrary state with respect to the bus -- and we can therefore come to
/// life with a transaction already in flight.  It is very important that we
/// abort any such transaction:  failure to do so will result in our first I2C
/// transaction being corrupted.  (And because our first I2C transaction on SP
/// boot may well be to disable segments on a mux, this can result in nearly
/// arbitrary mayhem down the road!)  To do this, we engage in the
/// time-honored[0] tradition of "clocking through the problem":  wiggling SCL
/// until we see SDA high, and then pulling SDA low and releasing SCL to
/// indicate a STOP condition.  (Note that we need to do this up to 9 times to
/// assure that we have clocked through the entire transaction.)  Our assumption
/// is that if SCL is being stretched by an errant target, it has been already
/// stretched beyond our timeout (25ms); if this is the case, us trying to
/// wiggle SCL here won't actually wiggle SCL -- but unless such a device is
/// isolated to a segment on a mux that we can reset, nothing will in fact help.
///
/// [0] Analog Devices. AN-686: Implementing an I2C Reset. 2003.
///
fn wiggle_scl(sys: &Sys, scl: PinSet, sda: PinSet) {
    let mut wiggles = 0_u8;
    sys.gpio_set(scl);

    sys.gpio_configure_output(
        scl,
        OutputType::OpenDrain,
        Speed::Low,
        Pull::None,
    );

    for _ in 0..9 {
        sys.gpio_set(sda);

        sys.gpio_configure_output(
            sda,
            OutputType::OpenDrain,
            Speed::Low,
            Pull::None,
        );

        sys.gpio_configure_input(sda, Pull::None);

        if sys.gpio_read(sda) == 0 {
            //
            // SDA is low -- someone is holding it down: give SCL a wiggle to
            // try to shake them.  Note that we don't sleep here:  we are
            // relying on the fact that communicating to the GPIO task is going
            // to take longer than our minimum SCL pulse.  (Which, on a 400 MHz
            // H753, is on the order of ~15 usecs -- yielding a cycle time of
            // ~30 usecs or ~33 KHz.)
            //
            sys.gpio_reset(scl);
            sys.gpio_set(scl);
            wiggles = wiggles.wrapping_add(1);
        } else {
            //
            // SDA is high. We're going to flip it back to an output, pull the
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
        }
    }

    ringbuf_entry!(Trace::Wiggles(wiggles));
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
                    // forever here.  If we can't manage to get the segments
                    // disabled, we will put the bus into an unknown mux state
                    // -- which means a bus will be in the unknown state if we
                    // fail to disable all segments for all of its muxes.
                    //
                    // In terms of why we might see a resolvable reset: we
                    // have noticed an issue whereby the first I2C transaction
                    // on some buses (notably, those that share controllers
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
                        ringbuf_entry!(Trace::SegmentFailed(code.into()));

                        if reset_needed(code) && !reset_attempted {
                            reset(controller, mux.port, muxes, muxmap);
                            reset_attempted = true;
                            continue;
                        }

                        //
                        // We have failed, and then failed again after the
                        // reset.  Mark the bus as being in an unknown mux
                        // state, which will prevent its use until it's
                        // resolved.
                        //
                        let bus = (controller.controller, mux.port);
                        ringbuf_entry!(Trace::MuxUnknown(bus));
                        muxmap.insert(bus, MuxState::Unknown);
                    }

                    break;
                }
                Err(code) => {
                    ringbuf_entry!(Trace::ConfigureFailed(code.into()));
                    reset_if_needed(code, controller, mux.port, muxes, muxmap);
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
