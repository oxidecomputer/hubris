// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    cert::PersistIdSelfCertBuilder, csr::PersistIdCsrBuilder, CertSerialNumber,
    IntermediateCert, PersistIdCert, SeedBuf,
};
use dice_mfg_msgs::{MfgMessage, SerialNumber, SizedBlob};
use lib_lpc55_usart::{Read, Usart, Write};
use nb;
use salty::{constants::SECRETKEY_SEED_LENGTH, signature::Keypair};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub enum Error {
    MsgDecode,
    MsgBufFull,
    UsartRead,
    UsartWrite,
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PersistIdSeed([u8; SECRETKEY_SEED_LENGTH]);

impl PersistIdSeed {
    pub fn new(seed: [u8; SECRETKEY_SEED_LENGTH]) -> Self {
        Self(seed)
    }
}

impl SeedBuf for PersistIdSeed {
    fn as_bytes(&self) -> &[u8; SECRETKEY_SEED_LENGTH] {
        &self.0
    }
}

// data returned to caller by MFG
// serial_number is required to use DeviceId as embedded certificate authority
// (ECA) post MFG
pub struct DiceMfgState {
    // TODO: The CertSerialNumber here represents the serial number for the
    // PersistId CA. If this cert is signed by the manufacturing line it will
    // be given a serial number by the mfg CA. Certs then issued by PersistId
    // can start from cert serial number 0.
    // When PersistId cert is self signed it's serial number will be 0 and the
    // DeviceId cert that it issues will have a cert serial number of 1.
    // This field tracks this state.
    pub cert_serial_number: CertSerialNumber,
    pub serial_number: SerialNumber,
    pub persistid_cert: PersistIdCert,
    pub intermediate_cert: Option<IntermediateCert>,
}

pub trait DiceMfg {
    fn run(self) -> DiceMfgState;
}

pub struct SelfMfg<'a> {
    keypair: &'a Keypair,
}

impl<'a> SelfMfg<'a> {
    pub fn new(keypair: &'a Keypair) -> Self {
        Self { keypair }
    }
}

impl DiceMfg for SelfMfg<'_> {
    fn run(self) -> DiceMfgState {
        let mut cert_sn: CertSerialNumber = Default::default();
        let dname_sn = SerialNumber::try_from("0123456789a").unwrap();

        let persistid_cert = PersistIdSelfCertBuilder::new(
            &cert_sn.next(),
            &dname_sn,
            &self.keypair.public,
        )
        .sign(self.keypair);

        DiceMfgState {
            cert_serial_number: cert_sn,
            serial_number: dname_sn,
            // TODO: static assert deviceid_cert size < SizedBuf max
            persistid_cert: persistid_cert,
            intermediate_cert: None,
        }
    }
}

pub struct SerialMfg<'a> {
    keypair: &'a Keypair,
    usart: Usart<'a>,
    buf: [u8; MfgMessage::MAX_ENCODED_SIZE],
    serial_number: Option<SerialNumber>,
    persistid_cert: Option<PersistIdCert>,
    intermediate_cert: Option<IntermediateCert>,
}

impl<'a> SerialMfg<'a> {
    pub fn new(keypair: &'a Keypair, usart: Usart<'a>) -> Self {
        Self {
            keypair,
            usart,
            buf: [0u8; MfgMessage::MAX_ENCODED_SIZE],
            serial_number: None,
            persistid_cert: None,
            intermediate_cert: None,
        }
    }

    /// The Break message is an indication from the mfg side of the comms
    /// that identity manufacturing is complete. We check this as best we
    /// (currently) can by ensuring all of the necessary data has been
    /// received.
    fn handle_break(&mut self) -> bool {
        if self.serial_number.is_none()
            || self.persistid_cert.is_none()
            || self.intermediate_cert.is_none()
        {
            let _ = self.send_nak();
            false
        } else {
            let _ = self.send_ack();
            true
        }
    }

    /// Handle a request for a CSR from the mfg system requires that we have
    /// already been given a serial number. If not we NAK the message.
    /// Otherwise we use the CSR builder to create a CSR that contains the
    /// serial number and identity public key. We then sign the CSR with the
    /// private part of the same key and send it back to the mfg system.
    fn handle_csrplz(&mut self) -> Result<(), Error> {
        if self.serial_number.is_none() {
            return self.send_nak();
        }

        let csr = PersistIdCsrBuilder::new(
            &self.serial_number.unwrap(),
            &self.keypair.public,
        )
        .sign(&self.keypair);

        self.send_csr(csr)
    }

    fn handle_persistid_cert(&mut self, cert: SizedBlob) -> Result<(), Error> {
        self.persistid_cert = Some(PersistIdCert(cert));

        self.send_ack()
    }

    fn handle_intermediate_cert(
        &mut self,
        cert: SizedBlob,
    ) -> Result<(), Error> {
        self.intermediate_cert = Some(IntermediateCert(cert));

        self.send_ack()
    }

    /// Store the serial number provided by the mfg system. If we've already
    /// received a cert for the identity we invalidate it. This is to prevent
    /// the mfg side from changing the SN after we've used it to create the
    /// identity cert.
    fn handle_serial_number(
        &mut self,
        serial_number: SerialNumber,
    ) -> Result<(), Error> {
        if self.persistid_cert.is_some() {
            self.persistid_cert = None;
        }

        self.serial_number = Some(serial_number);

        self.send_ack()
    }

    fn send_ack(&mut self) -> Result<(), Error> {
        self.send_msg(MfgMessage::Ack)
    }

    fn send_csr(&mut self, csr: SizedBlob) -> Result<(), Error> {
        self.send_msg(MfgMessage::Csr(csr))
    }

    fn send_nak(&mut self) -> Result<(), Error> {
        self.send_msg(MfgMessage::Nak)
    }

    fn get_msg(&mut self) -> Result<MfgMessage, Error> {
        let buf = &mut self.buf;

        match read_until_zero(&mut self.usart, buf) {
            Ok(size) => {
                MfgMessage::decode(&buf[..size]).map_err(|_| Error::MsgDecode)
            }
            Err(_) => Err(Error::UsartRead),
        }
    }

    fn send_msg(&mut self, msg: MfgMessage) -> Result<(), Error> {
        self.buf.fill(0);

        let size = msg.encode(&mut self.buf).expect("encode msg");
        write_all(&mut self.usart, &self.buf[..size])
    }
}

impl DiceMfg for SerialMfg<'_> {
    fn run(mut self) -> DiceMfgState {
        loop {
            let msg = match self.get_msg() {
                Ok(msg) => msg,
                Err(_) => continue,
            };

            let _ = match msg {
                MfgMessage::Break => {
                    if self.handle_break() {
                        break;
                    } else {
                        continue;
                    }
                }
                MfgMessage::CsrPlz => self.handle_csrplz(),
                MfgMessage::IdentityCert(cert) => {
                    self.handle_persistid_cert(cert)
                }
                MfgMessage::IntermediateCert(cert) => {
                    self.handle_intermediate_cert(cert)
                }
                MfgMessage::Ping => self.send_ack(),
                MfgMessage::SerialNumber(sn) => self.handle_serial_number(sn),
                _ => continue,
            };
        }

        flush_all(&mut self.usart);

        DiceMfgState {
            cert_serial_number: Default::default(),
            serial_number: self.serial_number.unwrap(),
            persistid_cert: self.persistid_cert.unwrap(),
            intermediate_cert: Some(self.intermediate_cert.unwrap()),
        }
    }
}

/// Write all bytes in buf to usart fifo, poll if fifo is full.
/// NOTE: This does not guarantee transmission of all bytes. See flush_all.
fn write_all(usart: &mut Usart, src: &[u8]) -> Result<(), Error> {
    for b in src {
        let _ = nb::block!(usart.write(*b)).map_err(|_| Error::UsartWrite);
    }
    Ok(())
}

/// Poll the usart reading bytes into dst until a termination sequence is
/// found.
pub fn read_until_zero(
    usart: &mut Usart,
    dst: &mut [u8],
) -> Result<usize, Error> {
    if dst.is_empty() {
        panic!("invalid dst or term");
    }
    let mut pos = 0;
    loop {
        match nb::block!(usart.read()) {
            Ok(b) => {
                if pos > dst.len() - 1 {
                    return Err(Error::MsgBufFull);
                }
                dst[pos] = b;
                pos += 1;

                if b == 0 {
                    return Ok(pos);
                }
            }
            Err(_) => return Err(Error::UsartRead),
        };
    }
}

/// Like 'flush' from embedded-hal 'Write' trait but polls till the transmit
/// FIFO is empty.
pub fn flush_all(usart: &mut Usart) {
    // flush only returns WouldBlock and nb::block eats that
    let _ = nb::block!(usart.flush());
}
