//! Talking to the printer over the network.
//!
//! ```no_run
//! # fn main() -> Result<(), sr5900p::printer::PrinterError> {
//! # let label: sr5900p::label::Label = unimplemented!();
//! use sr5900p::printer::Printer;
//! use std::time::Duration;
//!
//! let printer = Printer::new("192.168.11.16".parse().unwrap()).timeout(Duration::from_secs(3));
//! println!("{:?}", printer.status()?);
//! printer.print(&label)?;
//! # Ok(())
//! # }
//! ```
pub mod protocol;

use crate::label::display::TapeDisplay;
use crate::label::Label;
use crate::Tape;
use std::io;
use std::io::prelude::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::net::UdpSocket;
use std::num::Wrapping;
use std::thread;
use std::time::Duration;
use std::time::Instant;

/// Printer state reported by the status request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// ready to print (also reported right after a print completes)
    Idle {
        tape: Tape,
    },
    NoTape,
    CoverOpen,
    Printing,
    /// a status body that is not understood yet (e.g. the tape ran out)
    Unknown([u8; 20]),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrinterError {
    #[error("printer did not respond to {step} within {timeout:?}")]
    Timeout {
        step: &'static str,
        timeout: Duration,
    },
    #[error("printing did not finish within {0:?}")]
    PrintTimeout(Duration),
    #[error("no tape is loaded")]
    NoTape,
    #[error("the cover is open")]
    CoverOpen,
    #[error("the printer is busy printing")]
    Busy,
    #[error("the label is for {label:?} tape but {loaded:?} tape is loaded")]
    TapeMismatch { label: Tape, loaded: Tape },
    #[error("unknown tape index {0:#04x}")]
    UnknownTape(u8),
    #[error("unexpected printer status {0:?}")]
    UnexpectedStatus(Status),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl PrinterError {
    fn from_status(status: Status) -> Self {
        match status {
            Status::NoTape => PrinterError::NoTape,
            Status::CoverOpen => PrinterError::CoverOpen,
            Status::Printing => PrinterError::Busy,
            s => PrinterError::UnexpectedStatus(s),
        }
    }
}

/// A printer on the network. Every call opens its own sockets.
/// Prints to the same printer from several threads are not coordinated with each other,
/// so callers serialize them.
#[derive(Clone, Debug)]
pub struct Printer {
    addr: SocketAddr,
    timeout: Duration,
    print_timeout: Duration,
}

impl Printer {
    pub const DEFAULT_PORT: u16 = 9100;

    pub fn new(ip: IpAddr) -> Self {
        Self {
            addr: SocketAddr::new(ip, Self::DEFAULT_PORT),
            timeout: Duration::from_secs(5),
            print_timeout: Duration::from_secs(120),
        }
    }

    /// Port for both the UDP control messages and the TCP raster stream.
    pub fn port(mut self, port: u16) -> Self {
        self.addr.set_port(port);
        self
    }

    /// Limit for each response, for connecting, and for sending the whole raster stream
    /// (default 5 s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Limit for the whole wait until printing finishes (default 120 s).
    /// A whole `print` call therefore ends within about `print_timeout + 7 × timeout + 1.5 s`.
    pub fn print_timeout(mut self, timeout: Duration) -> Self {
        self.print_timeout = timeout;
        self
    }

    pub fn status(&self) -> Result<Status, PrinterError> {
        protocol::status(&self.control_socket()?, self.addr)
    }

    /// Print a rendered label. It must have been rendered for the loaded tape.
    pub fn print(&self, label: &Label) -> Result<(), PrinterError> {
        self.print_stream(&gen_tcp_data(label.bitmap()), Some(label.tape()))
    }

    /// Print an already encoded raster stream (see [`gen_tcp_data`]).
    /// The printer must be idle, but the stream is not checked against the loaded tape.
    pub fn print_raw(&self, data: &[u8]) -> Result<(), PrinterError> {
        self.print_stream(data, None)
    }

    fn control_socket(&self) -> Result<UdpSocket, PrinterError> {
        let bind: SocketAddr = match self.addr {
            SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
            SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
        };
        let socket = UdpSocket::bind(bind)?;
        socket.set_read_timeout(Some(self.timeout))?;
        Ok(socket)
    }

    fn print_stream(&self, data: &[u8], label_tape: Option<Tape>) -> Result<(), PrinterError> {
        let socket = self.control_socket()?;
        let tape = match protocol::status(&socket, self.addr)? {
            Status::Idle { tape } => tape,
            s => return Err(PrinterError::from_status(s)),
        };
        if let Some(label_tape) = label_tape {
            if label_tape != tape {
                return Err(PrinterError::TapeMismatch {
                    label: label_tape,
                    loaded: tape,
                });
            }
        }
        log::info!("Tape is {:?}, start printing...", tape);
        if let Err(e) = protocol::start_print(&socket, self.addr) {
            if matches!(e, PrinterError::Timeout { .. }) {
                // the request may have arrived with only the reply lost; leave no session open
                let _ = protocol::stop_print(&socket, self.addr);
            }
            return Err(e);
        }
        // held until after stop print, so the TCP connection never closes inside the session
        let mut stream = None;
        let printed = self.send_and_wait(&socket, data, &mut stream);
        // the wait may have shortened the read timeout; stop print gets the full one
        socket.set_read_timeout(Some(self.timeout))?;
        // release the print session even when sending failed, otherwise the printer stays busy
        let stopped = protocol::stop_print(&socket, self.addr);
        drop(stream);
        let final_status = printed?;
        stopped?;
        match final_status {
            Status::Idle { .. } => Ok(()),
            s => Err(PrinterError::from_status(s)),
        }
    }

    /// Returns the first status that is no longer `Printing`.
    fn send_and_wait(
        &self,
        socket: &UdpSocket,
        data: &[u8],
        stream: &mut Option<TcpStream>,
    ) -> Result<Status, PrinterError> {
        // pauses between the steps of the print session; the printer has only been used with them
        const STEP_PAUSE: Duration = Duration::from_millis(500);
        const POLL_INTERVAL: Duration = Duration::from_millis(500);

        thread::sleep(STEP_PAUSE);
        let stream = stream.insert(
            TcpStream::connect_timeout(&self.addr, self.timeout)
                .map_err(|e| protocol::timeout_or_io(e, "connect", self.timeout))?,
        );
        thread::sleep(STEP_PAUSE);
        protocol::notify_data_stream(socket, self.addr)?;
        thread::sleep(STEP_PAUSE);
        self.send_raster(stream, data)?;

        log::info!("Print data is sent. Waiting...");
        // every sleep and status read is cut to what is left, so the wait ends at the deadline
        let deadline = Instant::now() + self.print_timeout;
        let left = || deadline.saturating_duration_since(Instant::now());
        loop {
            if left().is_zero() {
                return Err(PrinterError::PrintTimeout(self.print_timeout));
            }
            thread::sleep(POLL_INTERVAL.min(left()));
            // measured once: a second reading could reach zero, which set_read_timeout rejects
            let remaining = left();
            if remaining.is_zero() {
                return Err(PrinterError::PrintTimeout(self.print_timeout));
            }
            socket.set_read_timeout(Some(self.timeout.min(remaining)))?;
            let status = match protocol::status(socket, self.addr) {
                Err(PrinterError::Timeout { .. }) if left().is_zero() => {
                    return Err(PrinterError::PrintTimeout(self.print_timeout))
                }
                r => r?,
            };
            log::info!("{:?}", status);
            // a completed print is accepted even when the reply comes at the deadline:
            // the label is already printed, and an error would invite printing it twice
            if status != Status::Printing {
                return Ok(status);
            }
        }
    }

    /// `timeout` bounds the whole stream, not each write: a printer that keeps
    /// reading slowly would otherwise stretch the send indefinitely.
    fn send_raster(&self, stream: &mut TcpStream, data: &[u8]) -> Result<(), PrinterError> {
        let deadline = Instant::now() + self.timeout;
        let mut sent = 0;
        while sent < data.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(PrinterError::Timeout {
                    step: "send raster",
                    timeout: self.timeout,
                });
            }
            stream.set_write_timeout(Some(left))?;
            match stream.write(&data[sent..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(n) => sent += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(protocol::timeout_or_io(e, "send raster", self.timeout)),
            }
        }
        Ok(())
    }
}

/// Encode the label as the raster byte stream sent over TCP.
pub fn gen_tcp_data(td: &TapeDisplay) -> Vec<u8> {
    let mut tcp_data: Vec<u8> = Vec::new();
    tcp_data.append(&mut vec![27, 123, 3, 64, 64, 125]);
    tcp_data.append(&mut vec![27, 123, 7, 123, 0, 0, 83, 84, 34, 125]);
    // meaning not identified yet (possibly the half-cut setting)
    tcp_data.append(&mut vec![27, 123, 7, 67, 2, 2, 1, 1, 73, 125]);
    tcp_data.append(&mut vec![27, 123, 4, 68, 5, 73, 125]);
    tcp_data.append(&mut vec![27, 123, 3, 71, 71, 125]);

    let mut tape_len_bytes = (td.width as u32 + 4/* safe margin */)
        .to_le_bytes()
        .to_vec();
    let mut cmd_bytes = vec![76];
    cmd_bytes.append(&mut tape_len_bytes);
    let csum = cmd_bytes
        .iter()
        .map(|v| Wrapping(*v))
        .sum::<Wrapping<u8>>()
        .0;
    cmd_bytes.push(csum);
    cmd_bytes.push(0x7d);
    tcp_data.append(&mut vec![0x1b, 0x7b, cmd_bytes.len() as u8]);
    tcp_data.append(&mut cmd_bytes);

    tcp_data.append(&mut vec![27, 123, 5, 84, 42, 0, 126, 125]);
    tcp_data.append(&mut vec![27, 123, 4, 72, 5, 77, 125]);
    tcp_data.append(&mut vec![27, 123, 4, 115, 0, 115, 125]);

    let row_bytes = td.height.div_ceil(8);
    for y in 0..td.width {
        tcp_data.append(&mut vec![0x1b, 0x2e, 0, 0, 0, 1]);
        tcp_data.append(&mut (td.height as u16).to_le_bytes().to_vec());
        for xb in 0..row_bytes {
            let mut chunk = 0x00;
            for dx in 0..8 {
                let x = xb * 8 + (7 - dx);

                if td.get_pixel(td.width - 1 - y, x) {
                    chunk |= 1 << dx
                }
            }
            tcp_data.push(chunk);
        }
    }
    tcp_data.push(0x0c); // data end
    tcp_data.append(&mut vec![27, 123, 3, 64, 64, 125]);
    tcp_data
}
