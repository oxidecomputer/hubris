// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// XXX IsrCounts is just here for testing. It goes away before merge

use core::sync::atomic::{AtomicU32,Ordering};

#[repr(C)]
#[derive(Debug)]
pub struct IsrCounts {
    isr0_wdt_bod_flash: AtomicU32,
    isr1_smma0: AtomicU32,
    isr2_gpio_global_int0: AtomicU32,
    isr3_gpio_global_int1: AtomicU32,
    isr4_gpio_int0_irq0: AtomicU32,
    isr5_gpio_int0_irq1: AtomicU32,
    isr6_gpio_int0_irq2: AtomicU32,
    isr7_gpio_int0_irq3: AtomicU32,
    isr8_utick: AtomicU32,
    isr9_mrt: AtomicU32,
    isr10_ctimer0: AtomicU32,
    isr11_ctimer1: AtomicU32,
    isr12_sct: AtomicU32,
    isr13_ctimer3: AtomicU32,
    isr14_flexcomm_interface_0: AtomicU32,
    isr15_flexcomm_interface_1: AtomicU32,
    isr16_flexcomm_interface_2: AtomicU32,
    isr17_flexcomm_interface_3: AtomicU32,
    isr18_flexcomm_interface_4: AtomicU32,
    isr19_flexcomm_interface_5: AtomicU32,
    isr20_flexcomm_interface_6: AtomicU32,
    isr21_flexcomm_interface_7: AtomicU32,
    isr22_adc: AtomicU32,
    isr23_reserved: AtomicU32,
    isr24_acmp: AtomicU32,
    isr25_reserved: AtomicU32,
    isr26_reserved: AtomicU32,
    isr27_usb0_needclk: AtomicU32,
    isr28_usb0: AtomicU32,
    isr29_rtc: AtomicU32,
    isr30_reserved: AtomicU32,
    isr31_wakeup_irqn_or_mailbox: AtomicU32,
    isr32_gpio_int0_irq4: AtomicU32,
    isr33_gpio_int0_irq5: AtomicU32,
    isr34_gpio_int0_irq6: AtomicU32,
    isr35_gpio_int0_irq7: AtomicU32,
    isr36_ctimer2: AtomicU32,
    isr37_ctimer4: AtomicU32,
    isr38_osevtimer: AtomicU32,
    isr39_reserved: AtomicU32,
    isr40_reserved: AtomicU32,
    isr41_reserved: AtomicU32,
    isr42_sdio: AtomicU32,
    isr43_reserved: AtomicU32,
    isr44_reserved: AtomicU32,
    isr45_reserved: AtomicU32,
    isr46_usb1_phy: AtomicU32,
    isr47_usb1_usb1: AtomicU32,
    isr48_usb1_needclk: AtomicU32,
    isr49_hypervisor: AtomicU32,
    isr50_sgpio_int0_irq0: AtomicU32,
    isr51_sgpio_int0_irq1: AtomicU32,
    isr52_plu: AtomicU32,
    isr53_sec_vio: AtomicU32,
    isr54_hashcrypt: AtomicU32,
    isr55_casper: AtomicU32,
    isr56_puf: AtomicU32,
    isr57_pq: AtomicU32,
    isr58_sdma1: AtomicU32,
    isr59_hs_spi: AtomicU32,
}

impl IsrCounts {
    pub fn increment(irq_num: u32) {
        let irq_num = irq_num as usize;
        const MAX: usize = core::mem::size_of::<IsrCounts>()
            / core::mem::size_of::<AtomicU32>();
        if irq_num < MAX {
            let pointer =
                core::ptr::addr_of!(ISR_COUNTERS) as *const ();
            let array = unsafe { &*(pointer as *const [AtomicU32; MAX]) };
            array[irq_num].fetch_add(1, Ordering::SeqCst);
        }
    }
}

static mut ISR_COUNTERS: IsrCounts = IsrCounts {
    isr0_wdt_bod_flash: AtomicU32::new(0),
    isr1_smma0: AtomicU32::new(0),
    isr2_gpio_global_int0: AtomicU32::new(0),
    isr3_gpio_global_int1: AtomicU32::new(0),
    isr4_gpio_int0_irq0: AtomicU32::new(0),
    isr5_gpio_int0_irq1: AtomicU32::new(0),
    isr6_gpio_int0_irq2: AtomicU32::new(0),
    isr7_gpio_int0_irq3: AtomicU32::new(0),
    isr8_utick: AtomicU32::new(0),
    isr9_mrt: AtomicU32::new(0),
    isr10_ctimer0: AtomicU32::new(0),
    isr11_ctimer1: AtomicU32::new(0),
    isr12_sct: AtomicU32::new(0),
    isr13_ctimer3: AtomicU32::new(0),
    isr14_flexcomm_interface_0: AtomicU32::new(0),
    isr15_flexcomm_interface_1: AtomicU32::new(0),
    isr16_flexcomm_interface_2: AtomicU32::new(0),
    isr17_flexcomm_interface_3: AtomicU32::new(0),
    isr18_flexcomm_interface_4: AtomicU32::new(0),
    isr19_flexcomm_interface_5: AtomicU32::new(0),
    isr20_flexcomm_interface_6: AtomicU32::new(0),
    isr21_flexcomm_interface_7: AtomicU32::new(0),
    isr22_adc: AtomicU32::new(0),
    isr23_reserved: AtomicU32::new(0),
    isr24_acmp: AtomicU32::new(0),
    isr25_reserved: AtomicU32::new(0),
    isr26_reserved: AtomicU32::new(0),
    isr27_usb0_needclk: AtomicU32::new(0),
    isr28_usb0: AtomicU32::new(0),
    isr29_rtc: AtomicU32::new(0),
    isr30_reserved: AtomicU32::new(0),
    isr31_wakeup_irqn_or_mailbox: AtomicU32::new(0),
    isr32_gpio_int0_irq4: AtomicU32::new(0),
    isr33_gpio_int0_irq5: AtomicU32::new(0),
    isr34_gpio_int0_irq6: AtomicU32::new(0),
    isr35_gpio_int0_irq7: AtomicU32::new(0),
    isr36_ctimer2: AtomicU32::new(0),
    isr37_ctimer4: AtomicU32::new(0),
    isr38_osevtimer: AtomicU32::new(0),
    isr39_reserved: AtomicU32::new(0),
    isr40_reserved: AtomicU32::new(0),
    isr41_reserved: AtomicU32::new(0),
    isr42_sdio: AtomicU32::new(0),
    isr43_reserved: AtomicU32::new(0),
    isr44_reserved: AtomicU32::new(0),
    isr45_reserved: AtomicU32::new(0),
    isr46_usb1_phy: AtomicU32::new(0),
    isr47_usb1_usb1: AtomicU32::new(0),
    isr48_usb1_needclk: AtomicU32::new(0),
    isr49_hypervisor: AtomicU32::new(0),
    isr50_sgpio_int0_irq0: AtomicU32::new(0),
    isr51_sgpio_int0_irq1: AtomicU32::new(0),
    isr52_plu: AtomicU32::new(0),
    isr53_sec_vio: AtomicU32::new(0),
    isr54_hashcrypt: AtomicU32::new(0),
    isr55_casper: AtomicU32::new(0),
    isr56_puf: AtomicU32::new(0),
    isr57_pq: AtomicU32::new(0),
    isr58_sdma1: AtomicU32::new(0),
    isr59_hs_spi: AtomicU32::new(0),
};
