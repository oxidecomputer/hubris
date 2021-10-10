
use drv_spiflash_api::{Instruction, ResponseCode};
use super::quadspi::{FMode, SpiMode, CommandConfig};

// This file provides static mappings of abstract SPI instructions for MT25Q parts 
// accessed through an STM32H7xx QUADSPI controller. As such, it expresses SFDP data
// in terms of H7 QUADSPI controller register values.
//
// See:
// [Micron MT25Q family SFDP Tables](https://www.micron.com/-/media/client/global/documents/products/technical-note/nor-flash/tn2506_sfdp_for_mt25q.pdf)
// JEDEC Standard - Serial Flash Discoverable Parameters (SFDP) JESD216E



// Table 1: SFDP Header Structure
// Description, Byte, Address Bits, Data
// Data: 128Mb 256Mb 512Mb 1Gb 2Gb

// The incoming instruction can be mapped to a different instruction.
// This is particularly relevant for the FAST READ commands that can have several
// different opcodes based on the modes for the Instruction, Address, and Data
// phases of the command.
// Since many flash commands are standard, and the MT25Q command set is being used
// for the abstract command set, most mappings are going to be 1:1.
// The abstract set can be changed to a simple enumeration of common commands if
// that makes sense to do.
pub fn api_to_h7_sfdp<'a>(instruction: Instruction, cmd: &'a mut CommandConfig) -> Result<&'a CommandConfig, ResponseCode>  {
    match instruction {
        Instruction::JedecId => {
            cmd.instruction = instruction as u8; // mapped 1:1 for now
            cmd.ddrm = false;   // operating in single mode
            cmd.dhhc = false;   // n/a for single mode
            cmd.ddrh = false;   // single mode
            cmd.fmode = FMode::IndirectRead;
            cmd.imode = SpiMode::Single;
            cmd.admode = SpiMode::Skip;
            cmd.adsize = 0;
            cmd.dcycles = 0;    // XXX 3? find doc for this
            cmd.dmode = SpiMode::Single;
            cmd.sioo = false;   // n/a for this command
            Ok(cmd)
            // TODO: Should we document the expected 3 bytes to be returned?
        },
        _ => Err(ResponseCode::BadArg),
    }
}


/*
// This is information transscribed (using tabula and some hand editing)
// from the MT25Q SFDP document. The text below goes away once it has been
// properly integrated into useful code.

SFDP signature
00h 7:0 53h 53h 53h 53h 53h
01h 7:0 46h 46h 46h 46h 46h
02h 7:0 44h 44h 44h 44h 44h
03h 7:0 50h 50h 50h 50h 50h

// SFDP Offset 0x00-0x03
pub sfdp_signature: [u8] = [0x53, 0x46, 0x44, 0x50];
// SFDP Offset 0x04-0x05
pub sfdp_parameter_revision: [u8] = [0x06, 0x01];
pub sfdp_n_parameter_headers = 1;   // offset 0x06

Parameter revision Minor 04h 7:0 06h 06h 06h 06h 06h
Major 05h 7:0 01h 01h 01h 01h 01h
Number of parameter headers 06h 7:0 01h 01h 01h 01h 01h
Unused 07h 7:0 FFh FFh FFh FFh FFh

parameter_id: [u8] = [0x00, 0x06, 0x01, 0x10];

         Parameter ID(0) 08h 7:0 00h 00h 00h 00h 00h
Parameter minor revision 09h 7:0 06h 06h 06h 06h 06h
Parameter major revision 0Ah 7:0 01h 01h 01h 01h 01h
Parameter length (in DW) 0Bh 7:0 10h 10h 10h 10h 10h
Parameter table pointer
0Ch 7:0 30h 30h 30h 30h 30h
0Dh 7:0 00h 00h 00h 00h 00h
0Eh 7:0 00h 00h 00h 00h 00h
Parameter 1 ID MSB 0Fh 7:0 FFh FFh FFh FFh FFh
Parameter 2 ID LSB 10h 7:0 84h 84h 84h 84h 84h
Parameter revision Minor 11h 7:0 00h 00h 00h 00h 00h
Major 12h 7:0 01h 01h 01h 01h 01h
Parameter length (in DW) 13h 7:0 02h 02h 02h 02h 02h
Parameter table pointer
14h 7:0 80h 80h 80h 80h 80h
15h 7:0 00h 00h 00h 00h 00h
16h 7:0 00h 00h 00h 00h 00h
Parameter 2 ID MSB 17h 7:0 FFh FFh FFh FFh FFh
Notes: 1. Locations from 18h to 1Fh contain FFh for standard MPNs
2. Others locations from 20h to 2Fh contain FFh.
TN-25-06: Serial Flash Discovery Parameters for MT25Q Family
Serial Flash Data Parameter – Header Structure
CCMTD-1725822587-3605
tn25_06_sfdp_mt26q - Rev. D 01/2021 EN 2
Micron Technology, Inc. reserves the right to change products or specifications without notice.
© 2012 Micron Technology, Inc. All rights reserved.
Serial Flash Data Parameter – Basic Properties
Table 2: Parameter Table – Flash Basic Properties
Description
Byte
Address Bits
Data
128Mb 256Mb 512Mb 1Gb 2Gb
Minimum sector erase sizes
30h
1:0 01b 01b 01b 01b 01b
Write granularity 2 1 1 1 1 1
WRITE ENABLE command required
for writing to volatile status registers
3 0 0 0 0 0
WRITE ENABLE command selected
for writing to volatile status registers
4 0 0 0 0 0
Not used 7:5 111b 111b 111b 111b 111b
4KB ERASE command 31h 7:0 20h 20h 20h 20h 20h
Supports 1-1-2 FAST READ
32h
0 1 1 1 1 1
Address bytes 2:1 00b 01b 01b 01b 01b
Supports double transfer rate clocking
3 1 1 1 1 1
Supports 1-2-2 FAST READ 4 1 1 1 1 1
Supports 1-4-4 FAST READ 5 1 1 1 1 1
Supports 1-1-4 FAST READ 6 1 1 1 1 1
Not used 7 1 1 1 1 1
Reserved 33h 7:0 FFh FFh FFh FFh FFh
Flash size (bits)
34h 7:0 FFh FFh FFh FFh FFh
35h 7:0 FFh FFh FFh FFh FFh
36h 7:0 FFh FFh FFh FFh FFh
37h 7:0 07h 0Fh 1Fh 3Fh 7Fh
1-4-4 FAST READ dummy cycle
count
38h
4:0 01001b 01001b 01001b 01001b 01001b
1-4-4 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
1-4-4 FAST READ command code 39h 7:0 EBh EBh EBh EBh EBh
1-1-4 FAST READ dummy cycle
count
3Ah
4:0 00111b 00111b 00111b 00111b 00111b
1-1-4 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
1-1-4 FAST READ command code 3Bh 7:0 6Bh 6Bh 6Bh 6Bh 6Bh
1-1-2 FAST READ dummy cycle
count
3Ch
4:0 00111b 00111b 00111b 00111b 00111b
1-1-2 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
1-1-2 FAST READ command 3Dh 7:0 3Bh 3Bh 3Bh 3Bh 3Bh
TN-25-06: Serial Flash Discovery Parameters for MT25Q Family
Serial Flash Data Parameter – Basic Properties
CCMTD-1725822587-3605
tn25_06_sfdp_mt26q - Rev. D 01/2021 EN 3
Micron Technology, Inc. reserves the right to change products or specifications without notice.
© 2012 Micron Technology, Inc. All rights reserved.
Table 2: Parameter Table – Flash Basic Properties (Continued)
Description
Byte
Address Bits
Data
128Mb 256Mb 512Mb 1Gb 2Gb
1-2-2 FAST READ dummy cycle
count
3Eh
4:0 00111b 00111b 00111b 00111b 00111b
1-2-2 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
1-2-2 Command code 3Fh 7:0 BBh BBh BBh BBh BBh
Supports 2-2-2 FAST READ
40h
0 1 1 1 1 1
Reserved 3:1 111b 111b 111b 111b 111b
Supports 4-4-4 FAST READ 4 1 1 1 1 1
Reserved 7:5 111b 111b 111b 111b 111b
Reserved 43:41h 31:8 FFFFFFh FFFFFFh FFFFFFh FFFFFFh FFFFFFh
Reserved 45:44h 15:0 FFFFh FFFFh FFFFh FFFFh FFFFh
2-2-2 FAST READ dummy cycle
count
46h
4:0 00111b 00111b 00111b 00111b 00111b
2-2-2 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
2-2-2 FAST READ command code 47h 7:0 BBh BBh BBh BBh BBh
Reserved 49:48h 15:0 FFFFh FFFFh FFFFh FFFFh FFFFh
4-4-4 FAST READ dummy cycle
count
4Ah
4:0 01001b 01001b 01001b 01001b 01001b
4-4-4 FAST READ number of mode
bits
7:5 001b 001b 001b 001b 001b
4-4-4 FAST READ command code 4Bh 7:0 EBh EBh EBh EBh EBh
Sector Type 1 size 4Ch 7:0 0Ch 0Ch 0Ch 0Ch 0Ch
Sector Type 1 command code 4Dh 7:0 20h 20h 20h 20h 20h
Sector Type 2 size 4Eh 7:0 10h 10h 10h 10h 10h
Sector Type 2 code 4Fh 7:0 D8h D8h D8h D8h D8h
Sector Type 3 size 50h 7:0 0Fh 0Fh 0Fh 0Fh 0Fh
Sector Type 3 code 51h 7:0 52h 52h 52h 52h 52h
Sector Type 4 size 52h 7:0 00h 00h 00h 00h 00h
Sector Type 4 code 53h 7:0 00h 00h 00h 00h 00h
TN-25-06: Serial Flash Discovery Parameters for MT25Q Family
Serial Flash Data Parameter – Basic Properties
CCMTD-1725822587-3605
tn25_06_sfdp_mt26q - Rev. D 01/2021 EN 4
Micron Technology, Inc. reserves the right to change products or specifications without notice.
© 2012 Micron Technology, Inc. All rights reserved.
Table 2: Parameter Table – Flash Basic Properties (Continued)
Description
Byte
Address Bits
Data
128Mb 256Mb 512Mb 1Gb 2Gb
Multiplier from typical erase time to
maximum erase time
57h:54h
3:0 0100b 0100b 0100b 0100b 0100b
Sector Type 1 Erase, Typical time 8:4 00010b 00010b 00010b 00010b 00010b
10:9 01b 01b 01b 01b 01b
Sector Type 2 Erase, Typical time 15:11 01001b 01001b 01001b 01001b 01001b
17:16 01b 01b 01b 01b 01b
Sector Type 3 Erase, Typical time 22:18 00110b 00110b 00110b 00110b 00110b
24:23 01b 01b 01b 01b 01b
Sector Type 4 Erase, Typical time 29:25 00000b 00000b 00000b 00000b 00000b
31:30 00b 00b 00b 00b 00b
Multiplier from typical time to maximum time for page or byte PROGRAM
5Bh:58h
3:0 1011b 1011b 1011b 1011b 1011b
Page size 7:4 1000b 1000b 1000b 1000b 1000b
Page Progr Typical time 12:8 01110b 01110b 01110b 01110b 01110b
13 0b 0b 0b 0b 0b
Byte Program Typical time, first
byte)
17:14 1110b 1110b 1110b 1110b 1110b
18 0b 0b 0b 0b 0b
Byte Program Typical time, additional byte (1)
22:19 0000b 0000b 0000b 0000b 0000b
23 0b 0b 0b 0b 0b
Chip Erase, Typical time 28:24 01001b 10100b 00001b 00001b 00001b
30:29 10b 10b 11b 11b 11b
Reserved 31 1b 1b 1b 1b 1b
Prohibited operations during PROGRAM SUSPEND
5Fh:5Ch
3:0 1100b 1100b 1100b 1100b 1100b
Prohibited operations during ERASE
SUSPEND
7:4 1010b 1010b 1010b 1010b 1010b
Reserved 8 1b 1b 1b 1b 1b
PROGRAM RESUME to SUSPEND interval (2)
12:9 0000b 0000b 0000b 0000b 0000b
SUSPEND in progress program maximum latency
17:13 11000b 11000b 11000b 11000b 11000b
19:18 01b 01b 01b 01b 01b
ERASE RESUME to SUSPEND interval 23:20 0010b 0010b 0010b 0010b 0010b
SUSPEND in progress erase maximum latency
28:24 11000b 11000b 11000b 11000b 11000b
30:29 01b 01b 01b 01b 01b
SUSPEND RESUME supported 31 0b 0b 0b 0b 0b
PROGRAM RESUME command 60h 7:0 7Ah 7Ah 7Ah 7Ah 7Ah
PROGRAM SUSPEND command 61h 7:0 75h 75h 75h 75h 75h

*/
