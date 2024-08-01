// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{KszPhyPort, KszPort};
use userlib::FromPrimitive;

/// Offsets used to access MIB counters
/// (see Table 4-200 in the datasheet for details)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MIBCounter {
    /// Rx lo-priority (default) octet count, including bad packets.
    RxLoPriorityByte = 0x0,

    /// Rx hi-priority octet count, including bad packets.
    RxHiPriorityByte = 0x1,

    /// Rx undersize packets with good CRC.
    RxUndersizePkt = 0x2,

    /// Rx fragment packets with bad CRC, symbol errors or alignment errors.
    RxFragments = 0x3,

    /// Rx oversize packets with good CRC (maximum: 2000 bytes).
    RxOversize = 0x4,

    /// Rx packets longer than 1522 bytes with either CRC errors, alignment errors, or symbol errors (depends on max packet size setting).
    RxJabbers = 0x5,

    /// Rx packets w/ invalid data symbol and legal packet size.
    RxSymbolError = 0x6,

    /// Rx packets within (64,1522) bytes w/ an integral number of bytes and a bad CRC (upper limit depends on maximum packet size setting).
    RxCRCError = 0x7,

    /// Rx packets within (64,1522) bytes w/ a non-integral number of bytes and a bad CRC (upper limit depends on maximum packet size setting).
    RxAlignmentError = 0x8,

    /// Number of MAC control frames received by a port with 88-08h in EtherType field.
    RxControl8808Pkts = 0x9,

    /// Number of PAUSE frames received by a port. PAUSE frame is qualified with EtherType (88-08h), DA, control opcode (00-01), data length (64B minimum), and a valid CRC.
    RxPausePkts = 0xA,

    /// Rx good broadcast packets (not including error broadcast packets or valid multicast packets).
    RxBroadcast = 0xB,

    /// Rx good multicast packets (not including MAC control frames, error multicast packets or valid broadcast packets).
    RxMulticast = 0xC,

    /// Rx good unicast packets.
    RxUnicast = 0xD,

    /// Total Rx packets (bad packets included) that were 64 octets in length.
    Rx64Octets = 0xE,

    /// Total Rx packets (bad packets included) that are between 65 and 127 octets in length.
    Rx65to127Octets = 0xF,

    /// Total Rx packets (bad packets included) that are between 128 and 255 octets in length.
    Rx128to255Octets = 0x10,

    /// Total Rx packets (bad packets included) that are between 256 and 511 octets in length.
    Rx256to511Octets = 0x11,

    /// Total Rx packets (bad packets included) that are between 512 and 1023 octets in length.
    Rx512to1023Octets = 0x12,

    /// Total Rx packets (bad packets included) that are between 1024 and 2000 octets in length (upper limit depends on max packet size setting).
    Rx1024to2000Octets = 0x13,

    /// Tx lo-priority good octet count, including PAUSE packets.
    TxLoPriorityByte = 0x14,

    /// Tx hi-priority good octet count, including PAUSE packets.
    TxHiPriorityByte = 0x15,

    /// The number of times a collision is detected later than 512 bit-times into the Tx of a packet.
    TxLateCollision = 0x16,

    /// Number of PAUSE frames transmitted by a port.
    TxPausePkts = 0x17,

    /// Tx good broadcast packets (not including error broadcast or valid multicast packets).
    TxBroadcastPkts = 0x18,

    /// Tx good multicast packets (not including error multicast packets or valid broadcast packets).
    TxMulticastPkts = 0x19,

    /// Tx good unicast packets.
    TxUnicastPkts = 0x1A,

    /// Tx packets by a port for which the 1st Tx attempt is delayed due to the busy medium.
    TxDeferred = 0x1B,

    /// Tx total collision, half duplex only.
    TxTotalCollision = 0x1C,

    /// A count of frames for which Tx fails due to excessive collisions.
    TxExcessiveCollision = 0x1D,

    /// Successfully Tx frames on a port for which Tx is inhibited by exactly one collision.
    TxSingleCollision = 0x1E,

    /// Successfully Tx frames on a port for which Tx is inhibited by more than one collision.
    TxMultipleCollision = 0x1F,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
#[allow(non_camel_case_types)]
pub enum Register {
    // Table 4-2
    // Meticulously transcribed by copy-pasting and elaborate Vim macros
    /// Chip ID and Enable Register [15:0]
    CIDER = 0x000,
    /// Switch Global Control Register 1 [15:0]
    SGCR1 = 0x002,
    /// Switch Global Control Register 2 [15:0]
    SGCR2 = 0x004,
    /// Switch Global Control Register 3 [15:0]
    SGCR3 = 0x006,
    /// Switch Global Control Register 6 [15:0]
    SGCR6 = 0x00C,
    /// Switch Global Control Register 7 [15:0]
    SGCR7 = 0x00E,
    /// MAC Address Register 1 [15:0]
    MACAR1 = 0x010,
    /// MAC Address Register 2 [15:0]
    MACAR2 = 0x012,
    /// MAC Address Register 3 [15:0]
    MACAR3 = 0x014,
    /// TOS Priority Control Register 1 [15:0]
    TOSR1 = 0x016,
    /// TOS Priority Control Register 2 [15:0]
    TOSR2 = 0x018,
    /// TOS Priority Control Register 3 [15:0]
    TOSR3 = 0x01A,
    /// TOS Priority Control Register 4 [15:0]
    TOSR4 = 0x01C,
    /// TOS Priority Control Register 5 [15:0]
    TOSR5 = 0x01E,
    /// TOS Priority Control Register 6 [15:0]
    TOSR6 = 0x020,
    /// TOS Priority Control Register 7 [15:0]
    TOSR7 = 0x022,
    /// TOS Priority Control Register 8 [15:0]
    TOSR8 = 0x024,
    /// Indirect Access Data Register 1 [15:0]
    IADR1 = 0x026,
    /// Indirect Access Data Register 2 [15:0]
    IADR2 = 0x028,
    /// Indirect Access Data Register 3 [15:0]
    IADR3 = 0x02A,
    /// Indirect Access Data Register 4 [15:0]
    IADR4 = 0x02C,
    /// Indirect Access Data Register 5 [15:0]
    IADR5 = 0x02E,
    /// Indirect Access Control Register [15:0]
    IACR = 0x030,
    /// Power Management Control and Wake-up Event Status Register [15:0]
    PMCTRL = 0x032,
    /// Go Sleep Time Register [15:0]
    GST = 0x036,
    /// Clock Tree Power Down Control Register [15:0]
    CTPDC = 0x038,
    /// PHY 1 and MII Basic Control Register [15:0]
    P1MBCR = 0x04C,
    /// PHY 1 and MII Basic Status Register [15:0]
    P1MBSR = 0x04E,
    /// PHY 1 PHYID Low Register [15:0]
    PHY1ILR = 0x050,
    /// PHY 1 PHYID High Register [15:0]
    PHY1IHR = 0x052,
    /// PHY 1 Auto-Negotiation Advertisement Register [15:0]
    P1ANAR = 0x054,
    /// PHY 1 Auto-Negotiation Link Partner Ability Register [15:0]
    P1ANLPR = 0x056,
    /// PHY 2 and MII Basic Control Register [15:0]
    P2MBCR = 0x058,
    /// PHY 2 and MII Basic Status Register [15:0]
    P2MBSR = 0x05A,
    /// PHY 2 PHYID Low Register [15:0]
    PHY2ILR = 0x05C,
    /// PHY 2 PHYID High Register [15:0]
    PHY2IHR = 0x05E,
    /// PHY 2 Auto-Negotiation Advertisement Register [15:0]
    P2ANAR = 0x060,
    /// PHY 2 Auto-Negotiation Link Partner Ability Register [15:0]
    P2ANLPR = 0x062,
    /// PHY 1 Special Control and Status Register [15:0]
    P1PHYCTRL = 0x066,
    /// PHY2 Special Control and Status Register [15:0]
    P2PHYCTRL = 0x06A,
    /// Port 1 Control Register 1 [15:0]
    P1CR1 = 0x06C,
    /// Port 1 Control Register 2 [15:0]
    P1CR2 = 0x06E,
    /// Port 1 VID Control Register [15:0]
    P1VIDCR = 0x070,
    /// Port 1 Control Register 3 [15:0]
    P1CR3 = 0x072,
    /// Port 1 Ingress Rate Control Register 0 [15:0]
    P1IRCR0 = 0x074,
    /// Port 1 Ingress Rate Control Register 1 [15:0]
    P1IRCR1 = 0x076,
    /// Port 1 Egress Rate Control Register 0 [15:0]
    P1ERCR0 = 0x078,
    /// Port 1 Egress Rate Control Register 1 [15:0]
    P1ERCR1 = 0x07A,
    /// Port 1 PHY Special Control/Status, LinkMD Register [15:0]
    P1SCSLMD = 0x07C,
    /// Port 1 Control Register 4 [15:0]
    P1CR4 = 0x07E,
    /// Port 1 Status Register [15:0]
    P1SR = 0x080,
    /// Port 2 Control Register 1 [15:0]
    P2CR1 = 0x084,
    /// Port 2 Control Register 2 [15:0]
    P2CR2 = 0x086,
    /// Port 2 VID Control Register [15:0]
    P2VIDCR = 0x088,
    /// Port 2 Control Register 3 [15:0]
    P2CR3 = 0x08A,
    /// Port 2 Ingress Rate Control Register 0 [15:0]
    P2IRCR0 = 0x08C,
    /// Port 2 Ingress Rate Control Register 1 [15:0]
    P2IRCR1 = 0x08E,
    /// Port 2 Egress Rate Control Register 0 [15:0]
    P2ERCR0 = 0x090,
    /// Port 2 Egress Rate Control Register 1 [15:0]
    P2ERCR1 = 0x092,
    /// Port 2 PHY Special Control/Status, LinkMD Register [15:0]
    P2SCSLMD = 0x094,
    /// Port 2 Control Register 4 [15:0]
    P2CR4 = 0x096,
    /// Port 2 Status Register [15:0]
    P2SR = 0x098,
    /// Port 3 Control Register 1 [15:0]
    P3CR1 = 0x09C,
    /// Port 3 Control Register 2 [15:0]
    P3CR2 = 0x09E,
    /// Port 3 VID Control Register [15:0]
    P3VIDCR = 0x0A0,
    /// Port 3 Control Register 3 [15:0]
    P3CR3 = 0x0A2,
    /// Port 3 Ingress Rate Control Register 0 [15:0]
    P3IRCR0 = 0x0A4,
    /// Port 3 Ingress Rate Control Register 1 [15:0]
    P3IRCR1 = 0x0A6,
    /// Port 3 Egress Rate Control Register 0 [15:0]
    P3ERCR0 = 0x0A8,
    /// Port 3 Egress Rate Control Register 1 [15:0]
    P3ERCR1 = 0x0AA,
    /// Switch Global Control Register 8 [15:0]
    SGCR8 = 0x0AC,
    /// Switch Global Control Register 9 [15:0]
    SGCR9 = 0x0AE,
    /// Source Address Filtering MAC Address 1 Register Low [15:0]
    SAFMACA1L = 0x0B0,
    /// Source Address Filtering MAC Address 1 Register Middle [15:0]
    SAFMACA1M = 0x0B2,
    /// Source Address Filtering MAC Address 1 Register High [15:0]
    SAFMACA1H = 0x0B4,
    /// Source Address Filtering MAC Address 2 Register Low [15:0]
    SAFMACA2L = 0x0B6,
    /// Source Address Filtering MAC Address 2 Register Middle [15:0]
    SAFMACA2M = 0x0B8,
    /// Source Address Filtering MAC Address 2 Register High [15:0]
    SAFMACA2H = 0x0BA,
    /// Port 1 TXQ Rate Control Register 1 [15:0]
    P1TXQRCR1 = 0x0C8,
    /// Port 1 TXQ Rate Control Register 2 [15:0]
    P1TXQRCR2 = 0x0CA,
    /// Port 2 TXQ Rate Control Register 1 [15:0]
    P2TXQRCR1 = 0x0CC,
    /// Port 2 TXQ Rate Control Register 2 [15:0]
    P2TXQRCR2 = 0x0CE,
    /// Port 3 TXQ Rate Control Register 1 [15:0]
    P3TXQRCR1 = 0x0D0,
    /// Port 3 TXQ Rate Control Register 2 [15:0]
    P3TXQRCR2 = 0x0D2,
    /// Input and Output Multiplex Selection Register [15:0]
    IOMXSEL = 0x0D6,
    /// Configuration Status and Serial Bus Mode Register [15:0]
    CFGR = 0x0D8,
    /// Port 1 Auto-Negotiation Next Page Transmit Register [15:0]
    P1ANPT = 0x0DC,
    /// Port 1 Auto-Negotiation Link Partner Received Next Page Register [15:0]
    P1ALPRNP = 0x0DE,
    /// Port 1 EEE and Link Partner Advertisement Register [15:0]
    P1EEEA = 0x0E0,
    /// Port 1 EEE Wake Error Count Register [15:0]
    P1EEEWEC = 0x0E2,
    /// Port 1 EEE Control/Status and Auto-Negotiation Expansion Register [15:0]
    P1EEECS = 0x0E4,
    /// Port 1 LPI Recovery Time Counter Register [7:0]
    P1LPIRTC = 0x0E6,
    /// Buffer Load to LPI Control 1 Register [7:0]
    BL2LPIC1 = 0x0E7,
    /// Port 2 Auto-Negotiation Next Page Transmit Register [15:0]
    P2ANPT = 0x0E8,
    /// Port 2 Auto-Negotiation Link Partner Received Next Page Register [15:0]
    P2ALPRNP = 0x0EA,
    /// Port 2 EEE and Link Partner Advertisement Register [15:0]
    P2EEEA = 0x0EC,
    /// Port 2 EEE Wake Error Count Register [15:0]
    P2EEEWEC = 0x0EE,
    /// Port 2 EEE Control/Status and Auto-Negotiation Expansion Register [15:0]
    P2EEECS = 0x0F0,
    /// Port 2 LPI Recovery Time Counter Register [7:0]
    P2LPIRTC = 0x0F2,
    /// PCS EEE Control Register [7:0]
    PCSEEEC = 0xF3,
    /// Empty TXQ to LPI Wait Time Control Register [15:0]
    ETLWTC = 0x0F4,
    /// Buffer Load to LPI Control 2 Register [15:0]
    BL2LPIC2 = 0x0F6,

    // Table 4-3
    /// Memory BIST Info Register [15:0]
    MBIR = 0x124,
    /// Global Reset Register [15:0]
    GRR = 0x126,
    /// Interrupt Enable Register [15:0]
    IER = 0x190,
    /// Interrupt Status Register [15:0]
    ISR = 0x192,

    // Table 4-4
    /// Trigger Output Unit Error Register [11:0]
    TRIG_ERR = 0x200,
    /// Trigger Output Unit Active Register [11:0]
    TRIG_ACTIVE = 0x202,
    /// Trigger Output Unit Done Register [11:0]
    TRIG_DONE = 0x204,
    /// Trigger Output Unit Enable Register [11:0]
    TRIG_EN = 0x206,
    /// Trigger Output Unit Software Reset Register [11:0]
    TRIG_SW_RST = 0x208,
    /// Trigger Output Unit 12 PPS Pulse Width Register
    TRIG12_PPS_WIDTH = 0x20A,
    /// Trigger Output Unit 1 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG1_TGT_NSL = 0x220,
    /// Trigger Output Unit 1 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG1_TGT_NSH = 0x222,
    /// Trigger Output Unit 1 Target Time in Seconds Low-Word Register [15:0]
    TRIG1_TGT_SL = 0x224,
    /// Trigger Output Unit 1 Target Time in Seconds High-Word Register [31:16]
    TRIG1_TGT_SH = 0x226,
    /// Trigger Output Unit 1 Configuration/Control Register1
    TRIG1_CFG_1 = 0x228,
    /// Trigger Output Unit 1 Configuration/Control Register2
    TRIG1_CFG_2 = 0x22A,
    /// Trigger Output Unit 1 Configuration/Control Register3
    TRIG1_CFG_3 = 0x22C,
    /// Trigger Output Unit 1 Configuration/Control Register4
    TRIG1_CFG_4 = 0x22E,
    /// Trigger Output Unit 1 Configuration/Control Register5
    TRIG1_CFG_5 = 0x230,
    /// Trigger Output Unit 1 Configuration/Control Register6
    TRIG1_CFG_6 = 0x232,
    /// Trigger Output Unit 1 Configuration/Control Register7
    TRIG1_CFG_7 = 0x234,
    /// Trigger Output Unit 1 Configuration/Control Register8
    TRIG1_CFG_8 = 0x236,
    /// Trigger Output Unit 2 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG2_TGT_NSL = 0x240,
    /// Trigger Output Unit 2 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG2_TGT_NSH = 0x242,
    /// Trigger Output Unit 2 Target Time in Seconds Low-Word Register [15:0]
    TRIG2_TGT_SL = 0x244,
    /// Trigger Output Unit 2 Target Time in Seconds High-Word Register [31:16]
    TRIG2_TGT_SH = 0x246,
    /// Trigger Output Unit 2 Configuration/Control Register1
    TRIG2_CFG_1 = 0x248,
    /// Trigger Output Unit 2 Configuration/Control Register2
    TRIG2_CFG_2 = 0x24A,
    /// Trigger Output Unit 2 Configuration/Control Register3
    TRIG2_CFG_3 = 0x24C,
    /// Trigger Output Unit 2 Configuration/Control Register4
    TRIG2_CFG_4 = 0x24E,
    /// Trigger Output Unit 2 Configuration/Control Register5
    TRIG2_CFG_5 = 0x250,
    /// Trigger Output Unit 2 Configuration/Control Register6
    TRIG2_CFG_6 = 0x252,
    /// Trigger Output Unit 2 Configuration/Control Register7
    TRIG2_CFG_7 = 0x254,
    /// Trigger Output Unit 2 Configuration/Control Register8
    TRIG2_CFG_8 = 0x256,
    /// Trigger Output Unit 3 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG3_TGT_NSL = 0x260,
    /// Trigger Output Unit 3 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG3_TGT_NSH = 0x262,
    /// Trigger Output Unit 3 Target Time in Seconds Low-Word Register [15:0]
    TRIG3_TGT_SL = 0x264,
    /// Trigger Output Unit 3 Target Time in Seconds High-Word Register [31:16]
    TRIG3_TGT_SH = 0x266,
    /// Trigger Output Unit 3 Configuration/Control Register1
    TRIG3_CFG_1 = 0x268,
    /// Trigger Output Unit 3 Configuration/Control Register2
    TRIG3_CFG_2 = 0x26A,
    /// Trigger Output Unit 3 Configuration/Control Register3
    TRIG3_CFG_3 = 0x26C,
    /// Trigger Output Unit 3 Configuration/Control Register4
    TRIG3_CFG_4 = 0x26E,
    /// Trigger Output Unit 3 Configuration/Control Register5
    TRIG3_CFG_5 = 0x270,
    /// Trigger Output Unit 3 Configuration/Control Register6
    TRIG3_CFG_6 = 0x272,
    /// Trigger Output Unit 3 Configuration/Control Register7
    TRIG3_CFG_7 = 0x274,
    /// Trigger Output Unit 3 Configuration/Control Register8
    TRIG3_CFG_8 = 0x276,
    /// Trigger Output Unit 4 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG4_TGT_NSL = 0x280,
    /// Trigger Output Unit 4 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG4_TGT_NSH = 0x282,
    /// Trigger Output Unit 4 Target Time in Seconds Low-Word Register [15:0]
    TRIG4_TGT_SL = 0x284,
    /// Trigger Output Unit 4 Target Time in Seconds High-Word Register [31:16]
    TRIG4_TGT_SH = 0x286,
    /// Trigger Output Unit 4 Configuration/Control Register1
    TRIG4_CFG_1 = 0x288,
    /// Trigger Output Unit 4 Configuration/Control Register2
    TRIG4_CFG_2 = 0x28A,
    /// Trigger Output Unit 4 Configuration/Control Register3
    TRIG4_CFG_3 = 0x28C,
    /// Trigger Output Unit 4 Configuration/Control Register4
    TRIG4_CFG_4 = 0x28E,
    /// Trigger Output Unit 4 Configuration/Control Register5
    TRIG4_CFG_5 = 0x290,
    /// Trigger Output Unit 4 Configuration/Control Register6
    TRIG4_CFG_6 = 0x292,
    /// Trigger Output Unit 4 Configuration/Control Register7
    TRIG4_CFG_7 = 0x294,
    /// Trigger Output Unit 4 Configuration/Control Register8
    TRIG4_CFG_8 = 0x296,
    /// Trigger Output Unit 5 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG5_TGT_NSL = 0x2A0,
    /// Trigger Output Unit 5 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG5_TGT_NSH = 0x2A2,
    /// Trigger Output Unit 5 Target Time in Seconds Low-Word Register [15:0]
    TRIG5_TGT_SL = 0x2A4,
    /// Trigger Output Unit 5 Target Time in Seconds High-Word Register [31:16]
    TRIG5_TGT_SH = 0x2A6,
    /// Trigger Output Unit 5 Configuration/Control Register1
    TRIG5_CFG_1 = 0x2A8,
    /// Trigger Output Unit 5 Configuration/Control Register2
    TRIG5_CFG_2 = 0x2AA,
    /// Trigger Output Unit 5 Configuration/Control Register3
    TRIG5_CFG_3 = 0x2AC,
    /// Trigger Output Unit 5 Configuration/Control Register4
    TRIG5_CFG_4 = 0x2AE,
    /// Trigger Output Unit 5 Configuration/Control Register5
    TRIG5_CFG_5 = 0x2B0,
    /// Trigger Output Unit 5 Configuration/Control Register6
    TRIG5_CFG_6 = 0x2B2,
    /// Trigger Output Unit 5 Configuration/Control Register7
    TRIG5_CFG_7 = 0x2B4,
    /// Trigger Output Unit 5 Configuration/Control Register8
    TRIG5_CFG_8 = 0x2B6,
    /// Trigger Output Unit 6 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG6_TGT_NSL = 0x2C0,
    /// Trigger Output Unit 6 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG6_TGT_NSH = 0x2C2,
    /// Trigger Output Unit 6 Target Time in Seconds Low-Word Register [15:0]
    TRIG6_TGT_SL = 0x2C4,
    /// Trigger Output Unit 6 Target Time in Seconds High-Word Register [31:16]
    TRIG6_TGT_SH = 0x2C6,
    /// Trigger Output Unit 6 Configuration/Control Register1
    TRIG6_CFG_1 = 0x2C8,
    /// Trigger Output Unit 6 Configuration/Control Register2
    TRIG6_CFG_2 = 0x2CA,
    /// Trigger Output Unit 6 Configuration/Control Register3
    TRIG6_CFG_3 = 0x2CC,
    /// Trigger Output Unit 6 Configuration/Control Register4
    TRIG6_CFG_4 = 0x2CE,
    /// Trigger Output Unit 6 Configuration/Control Register5
    TRIG6_CFG_5 = 0x2D0,
    /// Trigger Output Unit 6 Configuration/Control Register6
    TRIG6_CFG_6 = 0x2D2,
    /// Trigger Output Unit 6 Configuration/Control Register7
    TRIG6_CFG_7 = 0x2D4,
    /// Trigger Output Unit 6 Configuration/Control Register8
    TRIG6_CFG_8 = 0x2D6,
    /// Trigger Output Unit 7 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG7_TGT_NSL = 0x2E0,
    /// Trigger Output Unit 7 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG7_TGT_NSH = 0x2E2,
    /// Trigger Output Unit 7 Target Time in Seconds Low-Word Register [15:0]
    TRIG7_TGT_SL = 0x2E4,
    /// Trigger Output Unit 7 Target Time in Seconds High-Word Register [31:16]
    TRIG7_TGT_SH = 0x2E6,
    /// Trigger Output Unit 7 Configuration/Control Register1
    TRIG7_CFG_1 = 0x2E8,
    /// Trigger Output Unit 7 Configuration/Control Register2
    TRIG7_CFG_2 = 0x2EA,
    /// Trigger Output Unit 7 Configuration/Control Register3
    TRIG7_CFG_3 = 0x2EC,
    /// Trigger Output Unit 7 Configuration/Control Register4
    TRIG7_CFG_4 = 0x2EE,
    /// Trigger Output Unit 7 Configuration/Control Register5
    TRIG7_CFG_5 = 0x2F0,
    /// Trigger Output Unit 7 Configuration/Control Register6
    TRIG7_CFG_6 = 0x2F2,
    /// Trigger Output Unit 7 Configuration/Control Register7
    TRIG7_CFG_7 = 0x2F4,
    /// Trigger Output Unit 7 Configuration/Control Register8
    TRIG7_CFG_8 = 0x2F6,
    /// Trigger Output Unit 8 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG8_TGT_NSL = 0x300,
    /// Trigger Output Unit 8 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG8_TGT_NSH = 0x302,
    /// Trigger Output Unit 8 Target Time in Seconds Low-Word Register [15:0]
    TRIG8_TGT_SL = 0x304,
    /// Trigger Output Unit 8 Target Time in Seconds High-Word Register [31:16]
    TRIG8_TGT_SH = 0x306,
    /// Trigger Output Unit 8 Configuration/Control Register1
    TRIG8_CFG_1 = 0x308,
    /// Trigger Output Unit 8 Configuration/Control Register2
    TRIG8_CFG_2 = 0x30A,
    /// Trigger Output Unit 8 Configuration/Control Register3
    TRIG8_CFG_3 = 0x30C,
    /// Trigger Output Unit 8 Configuration/Control Register4
    TRIG8_CFG_4 = 0x30E,
    /// Trigger Output Unit 8 Configuration/Control Register5
    TRIG8_CFG_5 = 0x310,
    /// Trigger Output Unit 8 Configuration/Control Register6
    TRIG8_CFG_6 = 0x312,
    /// Trigger Output Unit 8 Configuration/Control Register7
    TRIG8_CFG_7 = 0x314,
    /// Trigger Output Unit 8 Configuration/Control Register8
    TRIG8_CFG_8 = 0x316,
    /// Trigger Output Unit 9 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG9_TGT_NSL = 0x320,
    /// Trigger Output Unit 9 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG9_TGT_NSH = 0x322,
    /// Trigger Output Unit 9 Target Time in Seconds Low-Word Register [15:0]
    TRIG9_TGT_SL = 0x324,
    /// Trigger Output Unit 9 Target Time in Seconds High-Word Register [31:16]
    TRIG9_TGT_SH = 0x326,
    /// Trigger Output Unit 9 Configuration/Control Register1
    TRIG9_CFG_1 = 0x328,
    /// Trigger Output Unit 9 Configuration/Control Register2
    TRIG9_CFG_2 = 0x32A,
    /// Trigger Output Unit 9 Configuration/Control Register3
    TRIG9_CFG_3 = 0x32C,
    /// Trigger Output Unit 9 Configuration/Control Register4
    TRIG9_CFG_4 = 0x32E,
    /// Trigger Output Unit 9 Configuration/Control Register5
    TRIG9_CFG_5 = 0x330,
    /// Trigger Output Unit 9 Configuration/Control Register6
    TRIG9_CFG_6 = 0x332,
    /// Trigger Output Unit 9 Configuration/Control Register7
    TRIG9_CFG_7 = 0x334,
    /// Trigger Output Unit 9 Configuration/Control Register8
    TRIG9_CFG_8 = 0x336,
    /// Trigger Output Unit 10 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG10_TGT_NSL = 0x340,
    /// Trigger Output Unit 10 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG10_TGT_NSH = 0x342,
    /// Trigger Output Unit 10 Target Time in Seconds Low-Word Register [15:0]
    TRIG10_TGT_SL = 0x344,
    /// Trigger Output Unit 10 Target Time in Seconds High-Word Register [31:16]
    TRIG10_TGT_SH = 0x346,
    /// Trigger Output Unit 10 Configuration/Control Register1
    TRIG10_CFG_1 = 0x348,
    /// Trigger Output Unit 10 Configuration/Control Register2
    TRIG10_CFG_2 = 0x34A,
    /// Trigger Output Unit 10 Configuration/Control Register3
    TRIG10_CFG_3 = 0x34C,
    /// Trigger Output Unit 10 Configuration/Control Register4
    TRIG10_CFG_4 = 0x34E,
    /// Trigger Output Unit 10 Configuration/Control Register5
    TRIG10_CFG_5 = 0x350,
    /// Trigger Output Unit 10 Configuration/Control Register6
    TRIG10_CFG_6 = 0x352,
    /// Trigger Output Unit 10 Configuration/Control Register7
    TRIG10_CFG_7 = 0x354,
    /// Trigger Output Unit 10 Configuration/Control Register8
    TRIG10_CFG_8 = 0x356,
    /// Trigger Output Unit 11 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG11_TGT_NSL = 0x360,
    /// Trigger Output Unit 11 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG11_TGT_NSH = 0x362,
    /// Trigger Output Unit 11 Target Time in Seconds Low-Word Register [15:0]
    TRIG11_TGT_SL = 0x364,
    /// Trigger Output Unit 11 Target Time in Seconds High-Word Register [31:16]
    TRIG11_TGT_SH = 0x366,
    /// Trigger Output Unit 11 Configuration/Control Register1
    TRIG11_CFG_1 = 0x368,
    /// Trigger Output Unit 11 Configuration/Control Register2
    TRIG11_CFG_2 = 0x36A,
    /// Trigger Output Unit 11 Configuration/Control Register3
    TRIG11_CFG_3 = 0x36C,
    /// Trigger Output Unit 11 Configuration/Control Register4
    TRIG11_CFG_4 = 0x36E,
    /// Trigger Output Unit 11 Configuration/Control Register5
    TRIG11_CFG_5 = 0x370,
    /// Trigger Output Unit 11 Configuration/Control Register6
    TRIG11_CFG_6 = 0x372,
    /// Trigger Output Unit 11 Configuration/Control Register7
    TRIG11_CFG_7 = 0x374,
    /// Trigger Output Unit 11 Configuration/Control Register8
    TRIG11_CFG_8 = 0x376,
    /// Trigger Output Unit 12 Target Time in Nanoseconds Low-Word Register [15:0]
    TRIG12_TGT_NSL = 0x380,
    /// Trigger Output Unit 12 Target Time in Nanoseconds High-Word Register [29:16]
    TRIG12_TGT_NSH = 0x382,
    /// Trigger Output Unit 12 Target Time in Seconds Low-Word Register [15:0]
    TRIG12_TGT_SL = 0x384,
    /// Trigger Output Unit 12 Target Time in Seconds High-Word Register [31:16]
    TRIG12_TGT_SH = 0x386,
    /// Trigger Output Unit 12 Configuration/Control Register1
    TRIG12_CFG_1 = 0x388,
    /// Trigger Output Unit 12 Configuration/Control Register2
    TRIG12_CFG_2 = 0x38A,
    /// Trigger Output Unit 12 Configuration/Control Register3
    TRIG12_CFG_3 = 0x38C,
    /// Trigger Output Unit 12 Configuration/Control Register4
    TRIG12_CFG_4 = 0x38E,
    /// Trigger Output Unit 12 Configuration/Control Register5
    TRIG12_CFG_5 = 0x390,
    /// Trigger Output Unit 12 Configuration/Control Register6
    TRIG12_CFG_6 = 0x392,
    /// Trigger Output Unit 12 Configuration/Control Register7
    TRIG12_CFG_7 = 0x394,
    /// Trigger Output Unit 12 Configuration/Control Register8
    TRIG12_CFG_8 = 0x396,

    // Table 4-5
    /// Input Unit Ready Register [11:0]
    TS_RDY = 0x400,
    /// Time stamp Input Unit Enable Register [11:0]
    TS_EN = 0x402,
    /// Time stamp Input Unit Software Reset Register [11:0]
    TS_SW_RST = 0x404,
    /// Time stamp Input Unit 1 Status Register
    TS1_STATUS = 0x420,
    /// Time stamp Input Unit 1 Configuration/Control Register
    TS1_CFG = 0x422,
    /// Time stamp Unit 1 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS1_SMPL1_NSL = 0x424,
    /// Time stamp Unit 1 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS1_SMPL1_NSH = 0x426,
    /// Time stamp Unit 1 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS1_SMPL1_SL = 0x428,
    /// Time stamp Unit 1 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS1_SMPL1_SH = 0x42A,
    /// Time stamp Unit 1 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS1_SMPL1_SUB_NS = 0x42C,
    /// Time stamp Unit 1 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS1_SMPL2_NSL = 0x434,
    /// Time stamp Unit 1 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS1_SMPL2_NSH = 0x436,
    /// Time stamp Unit 1 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS1_SMPL2_SL = 0x438,
    /// Time stamp Unit 1 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS1_SMPL2_SH = 0x43A,
    /// Time stamp Unit 1 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS1_SMPL2_SUB_NS = 0x43C,
    /// Time stamp Input Unit 2 Status Register
    TS2_STATUS = 0x440,
    /// Time stamp Input Unit 2 Configuration/Control Register
    TS2_CFG = 0x442,
    /// Time stamp Unit 2 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS2_SMPL1_NSL = 0x444,
    /// Time stamp Unit 2 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS2_SMPL1_NSH = 0x446,
    /// Time stamp Unit 2 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS2_SMPL1_SL = 0x448,
    /// Time stamp Unit 2 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS2_SMPL1_SH = 0x44A,
    /// Time stamp Unit 2 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS2_SMPL1_SUB_NS = 0x44C,
    /// Time stamp Unit 2 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS2_SMPL2_NSL = 0x454,
    /// Time stamp Unit 2 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS2_SMPL2_NSH = 0x456,
    /// Time stamp Unit 2 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS2_SMP2_SL = 0x458,
    /// Time stamp Unit 2 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS2_SMPL2_SH = 0x45A,
    /// Time stamp Unit 2 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS2_SMPL2_SUB_NS = 0x45C,
    /// Time stamp Input Unit 3 Status Register
    TS3_STATUS = 0x460,
    /// Time stamp Input Unit 3 Configuration/Control Register
    TS3_CFG = 0x462,
    /// Time stamp Unit 3 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS3_SMPL1_NSL = 0x464,
    /// Time stamp Unit 3 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS3_SMPL1_NSH = 0x466,
    /// Time stamp Unit 3 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS3_SMPL1_SL = 0x468,
    /// Time stamp Unit 3 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS3_SMPL1_SH = 0x46A,
    /// Time stamp Unit 3 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS3_SMPL1_SUB_NS = 0x46C,
    /// Time stamp Unit 3 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS3_SMPL2_NSL = 0x474,
    /// Time stamp Unit 3 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS3_SMPL2_NSH = 0x476,
    /// Time stamp Unit 3 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS3_SMP2_SL = 0x478,
    /// Time stamp Unit 3 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS3_SMPL2_SH = 0x47A,
    /// Time stamp Unit 3 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS3_SMPL2_SUB_NS = 0x47C,
    /// Time stamp Input Unit 4 Status Register
    TS4_STATUS = 0x480,
    /// Time stamp Input Unit 4 Configuration/Control Register
    TS4_CFG = 0x482,
    /// Time stamp Unit 4 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS4_SMPL1_NSL = 0x484,
    /// Time stamp Unit 4 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS4_SMPL1_NSH = 0x486,
    /// Time stamp Unit 4 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS4_SMPL1_SL = 0x488,
    /// Time stamp Unit 4 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS4_SMPL1_SH = 0x48A,
    /// Time stamp Unit 4 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS4_SMPL1_SUB_NS = 0x48C,
    /// Time stamp Unit 4 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS4_SMPL2_NSL = 0x494,
    /// Time stamp Unit 4 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS4_SMPL2_NSH = 0x496,
    /// Time stamp Unit 4 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS4_SMP2_SL = 0x498,
    /// Time stamp Unit 4 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS4_SMPL2_SH = 0x49A,
    /// Time stamp Unit 4 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS4_SMPL2_SUB_NS = 0x49C,
    /// Time stamp Input Unit 5 Status Register
    TS5_STATUS = 0x4A0,
    /// Time stamp Input Unit 5 Configuration/Control Register
    TS5_CFG = 0x4A2,
    /// Time stamp Unit 5 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS5_SMPL1_NSL = 0x4A4,
    /// Time stamp Unit 5 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS5_SMPL1_NSH = 0x4A6,
    /// Time stamp Unit 5 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS5_SMPL1_SL = 0x4A8,
    /// Time stamp Unit 5 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS5_SMPL1_SH = 0x4AA,
    /// Time stamp Unit 5 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS5_SMPL1_SUB_NS = 0x4AC,
    /// Time stamp Unit 5 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS5_SMPL2_NSL = 0x4B4,
    /// Time stamp Unit 5 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS5_SMPL2_NSH = 0x4B6,
    /// Time stamp Unit 5 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS5_SMP2_SL = 0x4B8,
    /// Time stamp Unit 5 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS5_SMPL2_SH = 0x4BA,
    /// Time stamp Unit 5 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS5_SMPL2_SUB_NS = 0x4BC,
    /// Time stamp Input Unit 6 Status Register
    TS6_STATUS = 0x4C0,
    /// Time stamp Input Unit 6 Configuration/Control Register
    TS6_CFG = 0x4C2,
    /// Time stamp Unit 6 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS6_SMPL1_NSL = 0x4C4,
    /// Time stamp Unit 6 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS6_SMPL1_NSH = 0x4C6,
    /// Time stamp Unit 6 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS6_SMPL1_SL = 0x4C8,
    /// Time stamp Unit 6 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS6_SMPL1_SH = 0x4CA,
    /// Time stamp Unit 6 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS6_SMPL1_SUB_NS = 0x4CC,
    /// Time stamp Unit 6 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS6_SMPL2_NSL = 0x4D4,
    /// Time stamp Unit 6 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS6_SMPL2_NSH = 0x4D6,
    /// Time stamp Unit 6 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS6_SMP2_SL = 0x4D8,
    /// Time stamp Unit 6 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS6_SMPL2_SH = 0x4DA,
    /// Time stamp Unit 6 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS6_SMPL2_SUB_NS = 0x4DC,
    /// Time stamp Input Unit 7 Status Register
    TS7_STATUS = 0x4E0,
    /// Time stamp Input Unit 7 Configuration/Control Register
    TS7_CFG = 0x4E2,
    /// Time stamp Unit 7 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS7_SMPL1_NSL = 0x4E4,
    /// Time stamp Unit 7 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS7_SMPL1_NSH = 0x4E6,
    /// Time stamp Unit 7 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS7_SMPL1_SL = 0x4E8,
    /// Time stamp Unit 7 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS7_SMPL1_SH = 0x4EA,
    /// Time stamp Unit 7 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS7_SMPL1_SUB_NS = 0x4EC,
    /// Time stamp Unit 7 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS7_SMPL2_NSL = 0x4F4,
    /// Time stamp Unit 7 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS7_SMPL2_NSH = 0x4F6,
    /// Time stamp Unit 7 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS7_SMP2_SL = 0x4F8,
    /// Time stamp Unit 7 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS7_SMPL2_SH = 0x4FA,
    /// Time stamp Unit 7 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS7_SMPL2_SUB_NS = 0x4FC,
    /// Time stamp Input Unit 8 Status Register
    TS8_STATUS = 0x500,
    /// Time stamp Input Unit 8 Configuration/Control Register
    TS8_CFG = 0x502,
    /// Time stamp Unit 8 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS8_SMPL1_NSL = 0x504,
    /// Time stamp Unit 8 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS8_SMPL1_NSH = 0x506,
    /// Time stamp Unit 8 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS8_SMPL1_SL = 0x508,
    /// Time stamp Unit 8 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS8_SMPL1_SH = 0x50A,
    /// Time stamp Unit 8 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS8_SMPL1_SUB_NS = 0x50C,
    /// Time stamp Unit 8 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS8_SMPL2_NSL = 0x514,
    /// Time stamp Unit 8 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS8_SMPL2_NSH = 0x516,
    /// Time stamp Unit 8 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS8_SMP2_SL = 0x518,
    /// Time stamp Unit 8 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS8_SMPL2_SH = 0x51A,
    /// Time stamp Unit 8 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS8_SMPL2_SUB_NS = 0x51C,
    /// Time stamp Input Unit 9 Status Register
    TS9_STATUS = 0x520,
    /// Time stamp Input Unit 9 Configuration/Control Register
    TS9_CFG = 0x522,
    /// Time stamp Unit 9 Input Sample Time (1st) in Nanoseconds High-Word Register [15:0]
    TS9_SMPL1_NSL = 0x524,
    /// Time stamp Unit 9 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS9_SMPL1_NSH = 0x526,
    /// Time stamp Unit 9 Input Sample Time (1st) in Seconds High-Word Register [15:0]
    TS9_SMPL1_SL = 0x528,
    /// Time stamp Unit 9 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS9_SMPL1_SH = 0x52A,
    /// Time stamp Unit 9 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS9_SMPL1_SUB_NS = 0x52C,
    /// Time stamp Unit 9 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS9_SMPL2_NSL = 0x534,
    /// Time stamp Unit 9 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS9_SMPL2_NSH = 0x536,
    /// Time stamp Unit 9 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS9_SMP2_SL = 0x538,
    /// Time stamp Unit 9 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS9_SMPL2_SH = 0x53A,
    /// Time stamp Unit 9 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS9_SMPL2_SUB_NS = 0x53C,
    /// Time stamp Input Unit 10 Status Register
    TS10_STATUS = 0x540,
    /// Time stamp Input Unit 10 Configuration/ Control Register
    TS10_CFG = 0x542,
    /// Time stamp Unit 10 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS10_SMPL1_NSL = 0x544,
    /// Time stamp Unit 10 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS10_SMPL1_NSH = 0x546,
    /// Time stamp Unit 10 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS10_SMPL1_SL = 0x548,
    /// Time stamp Unit 10 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS10_SMPL1_SH = 0x54A,
    /// Time stamp Unit 10 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS10_SMPL1_SUB_NS = 0x54C,
    /// Time stamp Unit 10 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS10_SMPL2_NSL = 0x554,
    /// Time stamp Unit 10 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS10_SMPL2_NSH = 0x556,
    /// Time stamp Unit 10 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS10_SMP2_SL = 0x558,
    /// Time stamp Unit 10 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS10_SMPL2_SH = 0x55A,
    /// Time stamp Unit 10 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS10_SMPL2_SUB_NS = 0x55C,
    /// Time stamp Input Unit 11 Status Register
    TS11_STATUS = 0x560,
    /// Time stamp Input Unit 11 Configuration/ Control Register
    TS11_CFG = 0x562,
    /// Time stamp Unit 11 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS11_SMPL1_NSL = 0x564,
    /// Time stamp Unit 11 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS11_SMPL1_NSH = 0x566,
    /// Time stamp Unit 11 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS11_SMPL1_SL = 0x568,
    /// Time stamp Unit 11 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS11_SMPL1_SH = 0x56A,
    /// Time stamp Unit 11 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS11_SMPL1_SUB_NS = 0x56C,
    /// Time stamp Unit 11 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS11_SMPL2_NSL = 0x574,
    /// Time stamp Unit 11 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS11_SMPL2_NSH = 0x576,
    /// Time stamp Unit 11 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS11_SMP2_SL = 0x578,
    /// Time stamp Unit 11 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS11_SMPL2_SH = 0x57A,
    /// Time stamp Unit 11 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS11_SMPL2_SUB_NS = 0x57C,
    /// Time stamp Input Unit 12 Status Register
    TS12_STATUS = 0x580,
    /// Time stamp Input Unit 12 Configuration/ Control Register
    TS12_CFG = 0x582,
    /// Time stamp Unit 12 Input Sample Time (1st) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL1_NSL = 0x584,
    /// Time stamp Unit 12 Input Sample Time (1st) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL1_NSH = 0x586,
    /// Time stamp Unit 12 Input Sample Time (1st) in Seconds Low-Word Register [15:0]
    TS12_SMPL1_SL = 0x588,
    /// Time stamp Unit 12 Input Sample Time (1st) in Seconds High-Word Register [31:16]
    TS12_SMPL1_SH = 0x58A,
    /// Time stamp Unit 12 Input Sample Time (1st) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL1_SUB_NS = 0x58C,
    /// Time stamp Unit 12 Input Sample Time (2nd) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL2_NSL = 0x594,
    /// Time stamp Unit 12 Input Sample Time (2nd) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL2_NSH = 0x596,
    /// Time stamp Unit 12 Input Sample Time (2nd) in Seconds Low-Word Register [15:0]
    TS12_SMP2_SL = 0x598,
    /// Time stamp Unit 12 Input Sample Time (2nd) in Seconds High-Word Register [31:16]
    TS12_SMPL2_SH = 0x59A,
    /// Time stamp Unit 12 Input Sample Time (2nd) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL2_SUB_NS = 0x59C,
    /// Time stamp Unit 12 Input Sample Time (3rd) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL3_NSL = 0x5A4,
    /// Time stamp Unit 12 Input Sample Time (3rd) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL3_NSH = 0x5A6,
    /// Time stamp Unit 12 Input Sample Time (3rd) in Seconds Low-Word Register [15:0]
    TS12_SMPL3_SL = 0x5A8,
    /// Time stamp Unit 12 Input Sample Time (3rd) in Seconds High-Word Register [31:16]
    TS12_SMPL3_SH = 0x5AA,
    /// Time stamp Unit 12 Input Sample Time (3rd) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL3_SUB_NS = 0x5AC,
    /// Time stamp Unit 12 Input Sample Time (4th) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL4_NSL = 0x5B4,
    /// Time stamp Unit 12 Input Sample Time (4th) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL4_NSH = 0x5B6,
    /// Time stamp Unit 12 Input Sample Time (4th) in Seconds Low-Word Register [15:0]
    TS12_SMPL4_SL = 0x5B8,
    /// Time stamp Unit 12 Input Sample Time (4th) in Seconds High-Word Register [31:16]
    TS12_SMPL4_SH = 0x5BA,
    /// Time stamp Unit 12 Input Sample Time (4th) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL4_SUB_NS = 0x5BC,
    /// Time stamp Unit 12 Input Sample Time (5th) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL5_NSL = 0x5C4,
    /// Time stamp Unit 12 Input Sample Time (5th) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL5_NSH = 0x5C6,
    /// Time stamp Unit 12 Input Sample Time (5th) in Seconds Low-Word Register [15:0]
    TS12_SMPL5_SL = 0x5C8,
    /// Time stamp Unit 12 Input Sample Time (5th) in Seconds High-Word Register [31:16]
    TS12_SMPL5_SH = 0x5CA,
    /// Time stamp Unit 12 Input Sample Time (5th) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL5_SUB_NS = 0x5CC,
    /// Time stamp Unit 12 Input Sample Time (6th) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL6_NSL = 0x5D4,
    /// Time stamp Unit 12 Input Sample Time (6th) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL6_NSH = 0x5D6,
    /// Time stamp Unit 12 Input Sample Time (6th) in Seconds Low-Word Register [15:0]
    TS12_SMPL6_SL = 0x5D8,
    /// Time stamp Unit 12 Input Sample Time (6th) in Seconds High-Word Register [31:16]
    TS12_SMPL6_SH = 0x5DA,
    /// Time stamp Unit 12 Input Sample Time (6th) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL6_SUB_NS = 0x5DC,
    /// Time stamp Unit 12 Input Sample Time (7th) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL7_NSL = 0x5E4,
    /// Time stamp Unit 12 Input Sample Time (7th) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL7_NSH = 0x5E6,
    /// Time stamp Unit 12 Input Sample Time (7th) in Seconds Low-Word Register [15:0]
    TS12_SMPL7_SL = 0x5E8,
    /// Time stamp Unit 12 Input Sample Time (7th) in Seconds High-Word Register [31:16]
    TS12_SMPL7_SH = 0x5EA,
    /// Time stamp Unit 12 Input Sample Time (7th) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL7_SUB_NS = 0x5EC,
    /// Time stamp Unit 12 Input Sample Time ( 8th) in Nanoseconds Low-Word Register [15:0]
    TS12_SMPL8_NSL = 0x5F4,
    /// Time stamp Unit 12 Input Sample Time (8th) in Nanoseconds High-Word Register [29:16]
    TS12_SMPL8_NSH = 0x5F6,
    /// Time stamp Unit 12 Input Sample Time (8th) in Seconds Low-Word Register [15:0]
    TS12_SMPL8_SL = 0x5F8,
    /// Time stamp Unit 12 Input Sample Time (8th) in Seconds High-Word Register [31:16]
    TS12_SMPL8_SH = 0x5FA,
    /// Time stamp Unit 12 Input Sample Time (8th) in Sub-Nanoseconds Register [2:0]
    TS12_SMPL8_SUB_NS = 0x5FC,

    // Table 6
    /// PTP Clock Control Register [6:0]
    PTP_CLK_CTL = 0x600,
    /// PTP Real Time Clock in Nanoseconds LowWord Register [15:0]
    PTP_RTC_NSL = 0x604,
    /// PTP Real Time Clock in Nanoseconds High-Word Register [31:16]
    PTP_RTC_NSH = 0x606,
    /// PTP Real Time Clock in Seconds LowWord Register [15:0]
    PTP_RTC_SL = 0x608,
    /// PTP Real Time Clock in Seconds HighWord Register [31:16]
    PTP_RTC_SH = 0x60A,
    /// PTP Real Time Clock in Phase Register [2:0]
    PTP_RTC_PHASE = 0x60C,
    /// PTP Sub-nanosecond Rate Low-Word Register [15:0]
    PTP_SNS_RATE_L = 0x610,
    /// PTP Sub-nanosecond Rate High-Word [29:16] and Configuration Register
    PTP_SNS_RATE_H = 0x612,
    /// PTP Temporary Adjustment Mode Duration Low-Word Register [15:0]
    PTP_TEMP_ADJ_DURA_L = 0x614,
    /// PTP Temporary Adjustment Mode Duration High-Word Register [31:16]
    PTP_TEMP_ADJ_DURA_H = 0x616,
    /// PTP Message Configuration 1 Register [7:0]
    PTP_MSG_CFG_1 = 0x620,
    /// PTP Message Configuration 2 Register [10:0]
    PTP_MSG_CFG_2 = 0x622,
    /// PTP Domain and Version Register [11:0]
    PTP_DOMAIN_VER = 0x624,
    /// PTP Port 1 Receive Latency Register [15:0]
    PTP_P1_RX_LATENCY = 0x640,
    /// PTP Port 1 Transmit Latency Register [15:0]
    PTP_P1_TX_LATENCY = 0x642,
    /// PTP Port 1 Asymmetry Correction Register [15:0]
    PTP_P1_ASYM_COR = 0x644,
    /// PTP Port 1 Link Delay Register [15:0]
    PTP_P1_LINK_DLY = 0x646,
    /// PTP Port 1 Egress Time stamp Low-Word for Pdelay_REQ and Delay_REQ Frames Register [15:0]
    P1_XDLY_REQ_TSL = 0x648,
    /// PTP Port 1 Egress Time stamp High-Word for Pdelay_REQ and Delay_REQ Frames Register [31:16]
    P1_XDLY_REQ_TSH = 0x64A,
    /// PTP Port 1 Egress Time stamp Low-Word for SYNC Frame Register [15:0]
    P1_SYNC_TSL = 0x64C,
    /// PTP Port 1 Egress Time stamp High-Word for SYNC Frame Register [31:16]
    P1_SYNC_TSH = 0x64E,
    /// PTP Port 1 Egress Time stamp Low-Word for Pdelay_resp Frame Register [15:0]
    P1_PDLY_RESP_TSL = 0x650,
    /// PTP Port 1 Egress Time stamp High-Word for Pdelay_resp Frame Register [31:16]
    P1_PDLY_RESP_TSH = 0x652,
    /// PTP Port 2 Receive Latency Register [15:0]
    PTP_P2_RX_LATENCY = 0x660,
    /// PTP Port 2 Transmit Latency Register [15:0]
    PTP_P2_TX_LATENCY = 0x662,
    /// PTP Port 2 Asymmetry Correction Register [15:0]
    PTP_P2_ASYM_COR = 0x664,
    /// PTP Port 2 Link Delay Register [15:0]
    PTP_P2_LINK_DLY = 0x666,
    /// PTP Port 2 Egress Time stamp Low-Word for Pdelay_REQ and Delay_REQ Frames Register [15:0]
    P2_XDLY_REQ_TSL = 0x668,
    /// PTP Port 2 Egress Time stamp High-Word for Pdelay_REQ and Delay_REQ Frames Register [31:16]
    P2_XDLY_REQ_TSH = 0x66A,
    /// PTP Port 2 Egress Time stamp Low-Word for SYNC Frame Register [15:0]
    P2_SYNC_TSL = 0x66C,
    /// PTP Port 2 Egress Time stamp High-Word for SYNC Frame Register [31:16]
    P2_SYNC_TSH = 0x66E,
    /// PTP Port 2 Egress Time stamp Low-Word for Pdelay_resp Frame Register [15:0]
    P2_PDLY_RESP_TSL = 0x670,
    /// PTP Port 2 Egress Time stamp High-Word for Pdelay_resp Frame Register [31:16]
    P2_PDLY_RESP_TSH = 0x672,
    /// PTP GPIO Monitor Register [11:0]
    GPIO_MONITOR = 0x680,
    /// PTP GPIO Output Enable Register [11:0]
    GPIO_OEN = 0x682,
    /// PTP Trigger Unit Interrupt Status Register
    PTP_TRIG_IS = 0x688,
    /// PTP Trigger Unit Interrupt Enable Register
    PTP_TRIG_IE = 0x68A,
    /// PTP Time stamp Unit Interrupt Status Register
    PTP_TS_IS = 0x68C,
    /// PTP Time stamp Unit Interrupt Enable Register
    PTP_TS_IE = 0x68E,
    /// DSP Control 1 Register
    DSP_CNTRL_6 = 0x734,
    /// Analog Control 1 Register
    ANA_CNTRL_1 = 0x748,
    /// Analog Control 3 Register
    ANA_CNTRL_3 = 0x74C,
}

#[allow(non_snake_case)]
impl Register {
    #[inline(always)]
    pub fn PxPHYCTRL(i: KszPhyPort) -> Self {
        Self::select2(i, Self::P1PHYCTRL, Self::P2PHYCTRL)
    }
    #[inline(always)]
    pub fn PxMBSR(i: KszPhyPort) -> Self {
        Self::select2(i, Self::P1MBSR, Self::P2MBSR)
    }
    #[inline(always)]
    pub fn PxMBCR(i: KszPhyPort) -> Self {
        Self::select2(i, Self::P1MBCR, Self::P2MBCR)
    }
    #[inline(always)]
    pub fn PxCR1(i: KszPort) -> Self {
        Self::select3(i, Self::P1CR1, Self::P2CR1, Self::P3CR1)
    }
    #[inline(always)]
    pub fn PxCR2(i: KszPort) -> Self {
        Self::select3(i, Self::P1CR2, Self::P2CR2, Self::P3CR2)
    }

    // Helper function to dispatch between two registers
    #[inline(always)]
    fn select2(i: KszPhyPort, r1: Register, r2: Register) -> Register {
        match i {
            KszPhyPort::One => r1,
            KszPhyPort::Two => r2,
        }
    }

    // Helper function to dispatch between three registers
    #[inline(always)]
    fn select3(
        i: KszPort,
        r1: Register,
        r2: Register,
        r3: Register,
    ) -> Register {
        match i {
            KszPort::One => r1,
            KszPort::Two => r2,
            KszPort::Three => r3,
        }
    }
}
