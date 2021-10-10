//! A driver for the STM32H7 SPI, in host mode.
//!
//! This is the core logic, separated from the IPC server. The peripheral also
//! supports I2S, which we haven't bothered implementing because we don't have a
//! need for it.
//!
//! # Clocking
//!
//! XXX The QUADSPI block has one clock domain.
//! 
//!  1. `ker_ck` contains the clock generator and is driven as a "kernel clock"
//!     from the RCC -- there is a separate mux there to choose its source.
//! 
//!  In host role, the QUADSPI needs to have at least `ker_ck` running to do useful
//!  work.
//! 
//!  # Automagic CRC generation
//! 
//!  We do not currently support the hardware's automatic CRC features.

// See https://docs.rs/stm32h7/0.13.0/stm32h7/stm32h743/quadspi/index.html

/// QuadSPI can handle single, dual, and quad-SPI protocols with the
/// same major configuration.
/// Using memory-mapped read-only access is more complicated and not yet implemented.
/// 
/// For Gimlet, a mux enable GPIO needs to be coordinated for shared access to
/// a flash part. That should be done at the qspi-server level or above.
///
/// Dual-flash mode is not implemented (two flash parts accessed in parallel).
/// See [AN4760 - Quad-SPI interface on STM32 microcontrollers and
/// microprocessors](https://www.st.com/resource/en/application_note/dm00227538-quadspi-interface-on-stm32-microcontrollers-and-microprocessors-stmicroelectronics.pdf)

use stm32h7::stm32h743 as device;
// use drv_spiflash_api as api;

// XXX figure out clock configuration.
//
// configured/generated with STM CubeMX
// see ~/my-stm23/Core/Src/quadspi.c

// Clock can come from HCLK3, PLL1Q, PLL2R, PER_CK
// hqspi.Instance = QUADSPI;
// hqspi.Init.ClockPrescaler = 2;
// hqspi.Init.FifoThreshold = 4;
// hqspi.Init.SampleShifting = QSPI_SAMPLE_SHIFTING_NONE;
// hqspi.Init.FlashSize = 16;
// hqspi.Init.ChipSelectHighTime = QSPI_CS_HIGH_TIME_2_CYCLE;
// hqspi.Init.ClockMode = QSPI_CLOCK_MODE_0;   
// hqspi.Init.FlashID = QSPI_FLASH_ID_1;
// hqspi.Init.DualFlash = QSPI_DUALFLASH_DISABLE;
//     PeriphClkInitStruct.PeriphClockSelection = RCC_PERIPHCLK_QSPI;
// PeriphClkInitStruct.QspiClockSelection = RCC_QSPICLKSOURCE_D1HCLK;
// see ~/my-stm23/Core/Src/main.c
// HAL_Init()
// SystemClock_Config()
// MX_GPIO_Init()
// MX_QUADSPI_Init();

//    /* QUADSPI clock enable */
//    __HAL_RCC_QSPI_CLK_ENABLE();
//
//    __HAL_RCC_GPIOE_CLK_ENABLE();
//    __HAL_RCC_GPIOF_CLK_ENABLE();
//    __HAL_RCC_GPIOB_CLK_ENABLE();
//
//    /* QUADSPI interrupt Init */
//    HAL_NVIC_SetPriority(QUADSPI_IRQn, 0, 0);
//    HAL_NVIC_EnableIRQ(QUADSPI_IRQn);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FMode {
    IndirectWrite = 0b00,
    // Communication starts immediately if:
    // • A write is performed to INSTRUCTION[7:0] (QUADSPI_CCR), if no address is required
    // (ADMODE = 00) and no data needs to be provided by the firmware (DMODE = 00)
    // • A write is performed to ADDRESS[31:0] (QUADSPI_AR), if an address is necessary
    // (ADMODE != 00) and if no data needs to be provided by the firmware (DMODE = 00)
    // • A write is performed to DATA[31:0] (QUADSPI_DR), if an address is necessary (when
    //  ADMODE != 00) and if data needs to be provided by the firmware (DMODE != 00)

    IndirectRead = 0b01,
    // Communication starts immediately if:
    // • A write is performed to INSTRUCTION [7:0] (QUADSPI_CCR), and if no address is
    // required (ADMODE=00)
    // • A write is performed to ADDRESS [31:0] (QUADSPI_AR), and if an address is
    // necessary (ADMODE!=00)

    #[allow(dead_code)]
    StatusFlagPolling = 0b10,
    // The accesses to the Flash memory begins in the same way as in the Indirect-read mode,
    // communication starts immediately if:
    // • A write is performed to INSTRUCTION [7:0] (QUADSPI_CCR) and if no address is
    // required (ADMODE=00)
    // • A write is performed to ADDRESS [31:0] (QUADSPI_AR) and if an address is
    // necessary (ADMODE!=00)

    #[allow(dead_code)]
    MemoryMapped = 0b11,
}

impl Default for FMode {
    fn default() -> Self { FMode::IndirectWrite }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SpiMode {
    Skip = 0,   // Skip this phase
    Single = 1,
    #[allow(dead_code)]
    Dual = 2,
    #[allow(dead_code)]
    Quad = 3
}

impl Default for SpiMode {
    fn default() -> Self { SpiMode::Skip }
}

// This structure is tied to the STM32h7 QUADSPI implemnetation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct CommandConfig {
    pub instruction: u8,
    pub ddrm: bool,       // Double Data Rate Mode for non-instruction phases.
    pub dhhc: bool,       // DDR Hold if in 2x mode, n/a for 1x
    pub ddrh: bool,       // If in DDR Mode,
    pub fmode: FMode,     // Data Phase mode
    pub imode: SpiMode,   // The instruction value is not constrained here.
    pub admode: SpiMode,  // Address mode
    pub adsize: u8,       // Number of bytes in optional address
    pub dcycles: u8,      // number of full clock dummy cycles to send (0..31)
    pub dmode: SpiMode,   // Data mode
    pub sioo: bool,       // Send Instruction on every transaction.
}

pub struct Qspi {
    /// Pointer to our register block.

    ///
    reg: &'static device::quadspi::RegisterBlock,
}

impl From<&'static device::quadspi::RegisterBlock> for Qspi {
    fn from(reg: &'static device::quadspi::RegisterBlock) -> Self {
        Self { reg }
    }
}

// AN4760 Rev 3, 3.3.1 Indirect mode
//  The amount of data to be transferred is set in the QUADSPI_DLR register
//  ... Automatic-polling mode is available to generate an interrupt...
//  In case of an erase or programming operation, the indirect mode has
// to be used and all the operations have to be handled by software.
//  In this case, it is recommended to use the Status-polling mode and
// then poll the status register inside the Flash memory to know when the
// programming or the erase operation is completed.
// AN4760 Rev 3, 3.3.1 Status-flag polling mode
//

impl Qspi {
    // Only one connected QSPI device is supported.
    // Having two flash devices working in parallel is not supported.

    // ST RM0433 23.3.3
    // The QUADSPI communicates with the Flash memory using commands. Each
    // command can include 5 phases: instruction, address, alternate byte,
    // dummy, data. Any of these phases can be configured to be skipped,
    // but at least one of the instruction, address, alternate byte, or
    // data phase must be present.
    //
    // The five communication phases are:
    //   - Instruction
    //   - Address
    //   - Alternate bytes
    //   - Dummy cycles
    //   - Data
    //
    pub fn enable(&mut self,
        cmd: &CommandConfig,
        address: Option<u32>,
        dlen: Option<u32>,
    ) {
        // XXX check for BUSY = 0
        self.reg.cr.write(|w| w.en().clear_bit());

        unsafe {
        self.reg.ccr.write(|w| w
            // fmode: 0=indirect write, 1=indirect read, 2=auto poll, 3=mmap
            //
            // Functional mode This field defines the QUADSPI functional
            // mode of operation. If DMAEN = 1 already, then the DMA
            // controller for the corresponding channel must be disabled
            // before changing the FMODE value. This field can be written
            // only when BUSY = 0."]
            .fmode().bits(cmd.fmode as u8)
            .ddrm().bit(cmd.ddrm)  // 2x or 1x data rate
            .dhhc().bit(cmd.dhhc)  // DDR Hold if in 2x mode, n/a for 1x
            .sioo().bit(cmd.sioo)  // 1=send inst on each transaction
            .imode().bits(cmd.imode as u8)  // Inst. 0=skip 1=single, 2=dual, 3=quad
            .instruction().bits(cmd.instruction as u8) // XXX starts command?
            .admode().bits(cmd.admode as u8)  // Address 0=skip 1=single, 2=dual, 3=quad
            .adsize().bits(cmd.adsize as u8) // Addr: 0=8-bit, 1=16, 2=24, 3=32-bit
            .abmode().bits(SpiMode::Skip as u8)  // AltBytes 0=skip 1=single, 2=dual, 3=quad
            .absize().bits(0 as u8)
            .dcyc().bits(cmd.dcycles as u8)
            .dmode().bits(cmd.dmode as u8)  // Data 0=skip 1=single, 2=dual, 3=quad
        );
        }

        if cmd.admode != SpiMode::Skip && address.is_some() {
            // XXX In some cases, writing the address starts the command.
            unsafe {
                self.reg.ar.write(|w| w.bits(address.unwrap()));
            }
        }
    }

    pub fn busy(self) -> bool {
        self.reg.sr.read().busy().bit()
    }

    // All commands have a one byte instruction
    // Some number of dummy cycles are needed for some instructions.
    // Some instructions require an address
    // Some instructions require a length (could be implicit?, e.g. 0x9f reads 3)
    //
    // Some commands will only work if the chip state has been set by
    // a previous command (e.g. WEN prior to PageProgram)
    // The status register needs the correct mode the for the command.
    //
    // ST RM0433 23.3.5
    //
    // Triggering the start of a command
    // Essentially, a command starts as soon as firmware gives the last
    // information that is necessary for this command.
    // Depending on the QUADSPI configuration, there are three different ways
    // to trigger the start of a command in indirect mode.
    //
    // The commands starts immediately after:
    // 1. a write is performed to INSTRUCTION[7:0] (QUADSPI_CCR),
    //    if no address is necessary (when ADMODE = 00) and if no data needs
    //    to be provided by the firmware (when FMODE = 01 or DMODE = 00)
    // 2. a write is performed to ADDRESS[31:0] (QUADSPI_AR), if an address
    //    is necessary (when ADMODE != 00) and if no data needs to be provided
    //    by the firmware (when FMODE = 01 or DMODE = 00)
    // 3. a write is performed to DATA[31:0] (QUADSPI_DR), if an address is
    //    necessary (when ADMODE != 00) and if data needs to be provided by the
    //    firmware (when FMODE = 00 and DMODE != 00)
    //
    // Writes to the alternate byte register (QUADSPI_ABR) never trigger the
    // communication start.
    // If alternate bytes are required, they must be programmed before.
    // As soon as a command is started, the BUSY bit (bit 5 of QUADSPI_SR)
    // is automatically set.
    pub fn inst_will_start(cmd: &CommandConfig) -> bool {
        cmd.admode == SpiMode::Skip &&
        (cmd.fmode == FMode::IndirectRead || cmd.dmode == SpiMode::Skip)
    }

    pub fn addr_will_start(cmd: &CommandConfig) -> bool {
        cmd.admode != SpiMode::Skip &&
        (cmd.fmode == FMode::IndirectRead || cmd.dmode == SpiMode::Skip)
    }

    pub fn data_will_start(cmd: &CommandConfig) -> bool {
        cmd.admode != SpiMode::Skip &&
        cmd.fmode == FMode::IndirectWrite &&
        cmd.dmode != SpiMode::Skip
    }

    pub fn start(&mut self, cmd: &CommandConfig) {
        // , xfer_len: u32, addr: u32
        self.end_of_transmission(); // Clear flag
        self.clear_transfer_interrupts(); // Clear EOT flag, etc.

        //if xfer_len > 0 {
        //    // 0xffff_ffff is an undefined length: xfer until end of flash
        //    unsafe {
        //        self.reg.dlr.write(|w| w.dl().bits(xfer_len + 1));
        //    }
        //}
        if cmd.admode != SpiMode::Skip {
            unsafe {
                self.reg.ccr.modify(|_, w| w
                    .admode().bits(cmd.admode as u8)
                    .adsize().bits(cmd.adsize)
                );
            }
            //unsafe {
            //    self.reg.ar.write(|w| w.bits(addr));
            //}
        }
        // self.reg.ar.write(|w| w.address().bits(address));
        // Trigger start of a command
        unsafe {
            self.reg.ccr.modify(|_, w| w.instruction().bits(cmd.instruction));
        }
    }

    pub fn can_rx_word(&self) -> bool {
        self.reg.sr.read().ftf().bit()  // FIFO threshold reached or data remains.
    }

    pub fn can_rx_byte(&self) -> bool {
        let sr = self.reg.sr.read();
        sr.ftf().bit() || sr.flevel().bits() > 0
    }

    pub fn can_tx_frame(&self) -> bool {
        let sr = self.reg.sr.read();
        // XXX
        !sr.ftf().bit() || sr.flevel().bits() < 32
    }

    pub fn send32(&mut self, bytes: u32) {
        // XXX xmit byte order?
        // XXX Upper limit on transfer?
        unsafe {
            self.reg.dr.write(|w| w.data().bits(bytes));
        }
    }

    pub fn end_of_transmission(&self) -> bool {
        // XXX does SR read clear this bit on QUADSPI?
        self.reg.sr.read().tcf().bit()
    }

    /// Stuffs one byte of data into the SPI TX FIFO.
    ///
    /// Preconditions:
    ///
    /// - There must be room for a byte in the TX FIFO (call `can_tx_frame` to
    ///   check, or call this in response to a TXP interrupt).
    pub fn send8(&mut self, byte: u8) {
        // The TXDR register can be accessed as a byte, halfword, or word. This
        // determines how many bytes are pushed in. stm32h7/svd2rust don't
        // understand this, and so we have to get a pointer to the byte portion
        // of the register manually and dereference it.

        // Because svd2rust didn't see this one coming, we cannot get a direct
        // reference to the VolatileCell within the wrapped Reg type of txdr,
        // nor will the Reg type agree to give us a pointer to its contents like
        // VolatileCell will, presumably to save us from ourselves. And thus we
        // must exploit the fact that VolatileCell is the only (non-zero-sized)
        // member of Reg, and in fact _must_ be for Reg to work correctly when
        // used to overlay registers in memory.

        // Safety: "Downcast" txdr to a pointer to its sole member, whose type
        // we know because of our unholy source-code-reading powers.
        let txdr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.dr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let txdr: *mut u32 = txdr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let txdr8 = txdr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe {
            txdr8.write_volatile(byte);
        }
    }

    pub fn recv32(&mut self) -> u32 {
        self.reg.dr.read().data().bits()
    }

    /// Pulls one byte of data from the SPI RX FIFO.
    ///
    /// Preconditions:
    ///
    /// - There must be at least one byte of data in the FIFO (check using
    ///   `has_rx_byte` or call this in response to an RXP interrupt).
    ///
    /// - Frame size must be set to 8 bits or smaller. (Behavior if you write a
    ///   partial frame to the FIFO is not immediately clear from the
    ///   datasheet.)
    pub fn recv8(&mut self) -> u8 {
        // The RXDR register can be accessed as a byte, halfword, or word. This
        // determines how many bytes are pushed in. stm32h7/svd2rust don't
        // understand this, and so we have to get a pointer to the byte portion
        // of the register manually and dereference it.

        // See send8 for further rationale / ranting.

        // Safety: "Downcast" rxdr to a pointer to its sole member, whose type
        // we know because of our unholy source-code-reading powers.
        let rxdr: &vcell::VolatileCell<u32> =
            unsafe { core::mem::transmute(&self.reg.dr) };
        // vcell is more pleasant and will happily give us the pointer we want.
        let rxdr: *mut u32 = rxdr.as_ptr();
        // As we are a little-endian machine it is sufficient to change the type
        // of the pointer to byte.
        let rxdr8 = rxdr as *mut u8;

        // Safety: we are dereferencing a pointer given to us by VolatileCell
        // (and thus UnsafeCell) using the same volatile access it would use.
        unsafe { rxdr8.read_volatile() }
    }

    pub fn end(&mut self) {
        // Clear flags that tend to get set during transactions.
        self.reg.fcr.write(|w| w.ctcf().set_bit());
        // Disable the transfer state machine.
        self.reg.cr.modify(|_, w| w
            .en().clear_bit()   // TODO: is this enough?
            // TODO: Do each of these need disable or just a main disable?
            .toie().clear_bit()
            .smie().clear_bit()
            .ftie().clear_bit()
            .tcie().clear_bit()
            .teie().clear_bit()
        );
        // Turn off interrupt enables.
        // self.reg.cr.reset(); // XXX bigger hammer: Reset CR?

        // This is where we'd report errors (TODO). For now, just clear the
        // error flags, as they're sticky.
        self.clear_transfer_interrupts();
    }

    pub fn clear_transfer_interrupts(&mut self) {
        self.reg.fcr.write(|w| { w
            .ctof().set_bit()   // clear timeout flag
            .csmf().set_bit()   // clear status match flag
            .ctcf().set_bit()   // clear transfer complete flag
            .ctef().set_bit()   // clear transfer error flag
        });
    }

    pub fn enable_transfer_interrupts(&mut self) {
        self.reg.cr.write(|w| w
            .toie().set_bit()   // TimeOut Interrupt Enable
            .ftie().set_bit()   // FIFO Threshold Interrupt Enable
            .tcie().set_bit()   // Transfer complete interrupt enable
            .teie().set_bit()); // Transfer error interrupt enable
    }

    pub fn disable_can_tx_interrupt(&mut self) {
        self.reg.cr.modify(|_, w| w.tcie().clear_bit());
    }

    pub fn check_eot(&self) -> bool {
        self.reg.sr.read().tcf().bit()
    }

    pub fn clear_eot(&mut self) {
        self.reg.cr.write(|w| w.tcie().set_bit());
    }
}
