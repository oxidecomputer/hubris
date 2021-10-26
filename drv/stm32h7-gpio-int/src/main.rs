#![no_std]
#![no_main]

use drv_stm32h7_rcc_api as rcc_api;
use ringbuf::*;
use stm32h7::stm32h7b3 as device;
use userlib::TaskId;
use userlib::*;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start,
    Valid(u32),
    Invalid(u32),
    None,
}

ringbuf!(Trace, 64, Trace::None);

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);

#[export_name = "main"]
fn main() -> ! {
    let gpio_driver = GPIO.get_task_id();
    let gpio_driver = drv_stm32h7_gpio_api::Gpio::from(gpio_driver);

    // E3 is our specific pin
    gpio_driver
        .configure_input(
            drv_stm32h7_gpio_api::Port::E.pin(3),
            drv_stm32h7_gpio_api::Pull::Down,
        )
        .unwrap();

    let rcc_driver = rcc_api::Rcc::from(RCC.get_task_id());

    rcc_driver.enable_clock(rcc_api::Peripheral::SysCfg);

    let syscfg = unsafe { &*device::SYSCFG::ptr() };

    // External pins get mapped e.g. A0, B0 -> exti0, A1, B1 -> exti1
    //
    // So E3 -> ext3 and E = 0b0100
    syscfg.exticr1.write(|w| unsafe { w.exti3().bits(0b0100) });

    let exti = unsafe { &*device::EXTI::ptr() };

    // Unmask interrupt line 3
    exti.cpuimr1.write(|w| w.mr3().set_bit());

    // Trigger on the rising edge on line 3
    exti.rtsr1.write(|w| w.tr3().set_bit());

    ringbuf_entry!(Trace::Start);

    loop {
        sys_irq_control(1, true);
        sys_recv_closed(&mut [], 1, TaskId::KERNEL).unwrap();

        // Check if line 3 is pending
        if exti.cpupr1.read().pr3().is_pending() {
            exti.cpupr1.write(|w| w.pr3().set_bit());
            ringbuf_entry!(Trace::Valid(exti.cpupr1.read().bits()));
        } else {
            ringbuf_entry!(Trace::Invalid(exti.cpupr1.read().bits()));
        }
    }
}
