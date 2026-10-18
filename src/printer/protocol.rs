//! UDP control messages on port 9100.
//!
//! Every request is a bare [`PacketHeader`] and every response is a header
//! followed by a command-specific body.
use super::PrinterError;
use super::Status;
use crate::Tape;
use std::boxed::Box;
use std::io;
use std::mem::size_of;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::slice;
use std::time::Duration;

/// # Safety
/// Implementing this trait is safe only when the target type can be converted
/// mutually between a byte sequence of the same size, which means that no ownership
/// nor memory references are involved.
pub unsafe trait Sliceable: Sized + Copy + Clone {
    fn copy_into_slice(&self) -> Box<[u8]> {
        let mut values = Box::<[u8]>::new_uninit_slice(size_of::<Self>());
        unsafe {
            values.copy_from_slice(slice::from_raw_parts(
                self as *const Self as *const MaybeUninit<u8>,
                size_of::<Self>(),
            ));
            values.assume_init()
        }
    }
}
unsafe impl Sliceable for PacketHeader {}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct PacketHeader {
    _signature: [u8; 4],  // "TPRT" for requests, "tprt" for responses
    _const00_be: [u8; 4], // 00 00 00 00
    _const01_be: [u8; 4], // 00 00 00 01
    _const20_be: [u8; 4], // 00 00 00 20
    _cmd_be: [u8; 4],
    _data_size_be: [u8; 4],
    _ip_addr_be: [u8; 4],
    _token_be: [u8; 4],
}
impl PacketHeader {
    pub fn new_request(cmd: u32, data_size: u32) -> Self {
        Self {
            _signature: *b"TPRT",
            _const00_be: 0x00u32.to_be_bytes(),
            _const01_be: 0x01u32.to_be_bytes(),
            _const20_be: 0x20u32.to_be_bytes(),
            _cmd_be: cmd.to_be_bytes(),
            _data_size_be: data_size.to_be_bytes(),
            _ip_addr_be: 0x00u32.to_be_bytes(),
            _token_be: 0x00u32.to_be_bytes(),
        }
    }
}

pub const CMD_STATUS: u32 = 0x0001;
pub const CMD_START_PRINT: u32 = 0x0002;
pub const CMD_STOP_PRINT: u32 = 0x0003;
/// sent first when the raster stream is about to start; the reply is ignored
pub const CMD_BEGIN_DATA_STREAM: u32 = 0x0101;
/// sent second; the printer replies 0x10 when it accepts the stream
pub const CMD_CONFIRM_DATA_STREAM: u32 = 0x0100;

/// Map a socket error to [`PrinterError::Timeout`] when its deadline (read, write or connect)
/// expired, and to [`PrinterError::Io`] otherwise.
pub(super) fn timeout_or_io(e: io::Error, step: &'static str, timeout: Duration) -> PrinterError {
    match e.kind() {
        // the platform reports an expired SO_RCVTIMEO as either of these
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            PrinterError::Timeout { step, timeout }
        }
        _ => PrinterError::Io(e),
    }
}

/// Send one request and return the body of the response.
/// The socket must have a read timeout set, otherwise this blocks forever on a silent printer.
pub(super) fn request(
    socket: &UdpSocket,
    addr: SocketAddr,
    cmd: u32,
    step: &'static str,
) -> Result<Vec<u8>, PrinterError> {
    let timeout = socket
        .read_timeout()?
        .expect("the control socket must have a read timeout");
    socket
        .send_to(&PacketHeader::new_request(cmd, 0).copy_into_slice(), addr)
        .map_err(|e| timeout_or_io(e, step, timeout))?;
    let mut buf = [0; 128];
    let (len, _) = socket
        .recv_from(&mut buf)
        .map_err(|e| timeout_or_io(e, step, timeout))?;
    if len < size_of::<PacketHeader>() {
        return Err(PrinterError::Protocol(format!(
            "{step}: response too short {:?}",
            &buf[..len]
        )));
    }
    // "tprt" comes from the protocol notes but has not been checked on a printer yet,
    // so a mismatch must not make the printer unusable
    if &buf[..4] != b"tprt" {
        log::warn!("{step}: unexpected response signature {:?}", &buf[..4]);
    }
    Ok(buf[size_of::<PacketHeader>()..len].to_vec())
}

pub(super) fn status(socket: &UdpSocket, addr: SocketAddr) -> Result<Status, PrinterError> {
    let body = request(socket, addr, CMD_STATUS, "status")?;
    log::debug!("status: {:?}", body);
    let data: [u8; 20] = body.as_slice().try_into().map_err(|_| {
        PrinterError::Protocol(format!("status: expected 20 bytes but got {}", body.len()))
    })?;
    parse_status(&data)
}

/// Interpret the 20-byte status body.
///
/// Observed bodies (byte 0x03 is the tape index):
/// - idle:               `[20, 0,  0, 4, 0, 0, 0, 0, 64, 0, 0,  0, 0, 0,  0, 0,  0, 0, 0, 0]`
/// - printing:           `[20, 2,  0, 4, 0, 0, 0, 0, 64, 0, 0,  0, 0, 0,  0, 0,  0, 0, 0, 0]`
/// - printing completed: `[20, 0,  0, 4, 0, 0, 0, 0, 64, 0, 0,  0, 0, 1,  0, 0,  0, 0, 0, 0]`
/// - tape exhausted:     `[20, 0, 66, 4, 0, 0, 0, 0, 64, 0, 0, 64, 0, 0, 66, 0, 64, 0, 0, 0]`
/// - unidentified:       `[20, 0,  0, 4, 0, 0, 0, 0, 64, 0, 0,  0, 0, 0, 66, 0, 64, 0, 0, 0]`
pub fn parse_status(data: &[u8; 20]) -> Result<Status, PrinterError> {
    Ok(match (data[0x01], data[0x0d]) {
        (2, 0) => Status::Printing,
        (0, 0..=2) => match data[0x02] {
            0x06 => Status::NoTape,
            0x21 => Status::CoverOpen,
            0x00 => Status::Idle {
                tape: match data[0x03] {
                    0x01 => Tape::W6,
                    0x02 => Tape::W9,
                    0x03 => Tape::W12,
                    0x04 => Tape::W18,
                    0x05 => Tape::W24,
                    0x06 => Tape::W36,
                    0x0B => Tape::W4,
                    ti => return Err(PrinterError::UnknownTape(ti)),
                },
            },
            _ => Status::Unknown(*data),
        },
        (v01, v0d) => {
            log::warn!("Unknown status v01: {v01}, v0d: {v0d}");
            Status::Unknown(*data)
        }
    })
}

pub(super) fn start_print(socket: &UdpSocket, addr: SocketAddr) -> Result<(), PrinterError> {
    expect_body(
        request(socket, addr, CMD_START_PRINT, "start print")?,
        &[2, 0, 0],
        "start print",
    )
}

pub(super) fn stop_print(socket: &UdpSocket, addr: SocketAddr) -> Result<(), PrinterError> {
    expect_body(
        request(socket, addr, CMD_STOP_PRINT, "stop print")?,
        &[3, 0, 0],
        "stop print",
    )
}

/// Tell the printer that the raster stream follows on the TCP connection.
pub(super) fn notify_data_stream(socket: &UdpSocket, addr: SocketAddr) -> Result<(), PrinterError> {
    request(socket, addr, CMD_BEGIN_DATA_STREAM, "begin data stream")?;
    let body = request(socket, addr, CMD_CONFIRM_DATA_STREAM, "confirm data stream")?;
    if body == [0x00] {
        log::warn!("Response for cmd 0x0100 was 0x00 (normally 0x10)");
        Ok(())
    } else {
        expect_body(body, &[0x10], "confirm data stream")
    }
}

fn expect_body(body: Vec<u8>, expected: &[u8], step: &'static str) -> Result<(), PrinterError> {
    if body == expected {
        Ok(())
    } else {
        Err(PrinterError::Protocol(format!(
            "{step}: expected {expected:?} but got {body:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_maps_observed_bodies() {
        let idle = |tape_index: u8| {
            let mut d = [20, 0, 0, 4, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            d[3] = tape_index;
            d
        };
        let mut printing = idle(4);
        printing[1] = 2;
        let mut completed = idle(4);
        completed[0x0d] = 1;
        let mut no_tape = idle(4);
        no_tape[2] = 0x06;
        let mut cover = idle(4);
        cover[2] = 0x21;
        let exhausted = [
            20, 0, 66, 4, 0, 0, 0, 0, 64, 0, 0, 64, 0, 0, 66, 0, 64, 0, 0, 0,
        ];
        let cases: Vec<([u8; 20], Status)> = vec![
            (idle(0x01), Status::Idle { tape: Tape::W6 }),
            (idle(0x02), Status::Idle { tape: Tape::W9 }),
            (idle(0x03), Status::Idle { tape: Tape::W12 }),
            (idle(0x04), Status::Idle { tape: Tape::W18 }),
            (idle(0x05), Status::Idle { tape: Tape::W24 }),
            (idle(0x06), Status::Idle { tape: Tape::W36 }),
            (idle(0x0B), Status::Idle { tape: Tape::W4 }),
            (printing, Status::Printing),
            (completed, Status::Idle { tape: Tape::W18 }),
            (no_tape, Status::NoTape),
            (cover, Status::CoverOpen),
            (exhausted, Status::Unknown(exhausted)),
        ];
        for (data, expected) in cases {
            assert_eq!(parse_status(&data).unwrap(), expected, "{data:?}");
        }
    }

    #[test]
    fn parse_status_rejects_unknown_tape_index() {
        let d = [
            20, 0, 0, 0x07, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert!(matches!(
            parse_status(&d),
            Err(PrinterError::UnknownTape(0x07))
        ));
    }
}
