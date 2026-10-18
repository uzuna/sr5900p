//! Talking to the printer over the network.
pub mod analyzer;
pub mod protocol;

use crate::label::display::TapeDisplay;
use crate::PrinterStatus;
use crate::Tape;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use protocol::notify_data_stream;
use protocol::StartPrintRequest;
use protocol::StatusRequest;
use protocol::StopPrintRequest;
use std::io::prelude::Write;
use std::net::TcpStream;
use std::net::UdpSocket;
use std::num::Wrapping;
use std::thread;
use std::time;

/// Send a raw byte stream to the printer and wait until printing finishes.
pub fn print_tcp_data(device_ip: &str, data: &[u8]) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("failed to bind")?;
    let info = StatusRequest::send(&socket, device_ip)?;
    println!("{:?}", info);
    if let PrinterStatus::SomeTape(t) = info {
        println!("Tape is {:?}, start printing...", t);
    } else {
        return Err(anyhow!("Unexpected state. Aborting... status: {:?}", info));
    }
    StartPrintRequest::send(&socket, device_ip)?;
    thread::sleep(time::Duration::from_millis(500));
    let mut stream = TcpStream::connect(device_ip.to_string() + ":9100")?;
    thread::sleep(time::Duration::from_millis(500));
    notify_data_stream(&socket, device_ip)?;
    thread::sleep(time::Duration::from_millis(500));
    stream.write_all(data)?;

    println!("Print data is sent. Waiting...");
    loop {
        thread::sleep(time::Duration::from_millis(500));
        let info = StatusRequest::send(&socket, device_ip)?;
        println!("{:?}", info);
        if let PrinterStatus::Printing = info {
            continue;
        }
        break;
    }

    StopPrintRequest::send(&socket, device_ip)?;

    Ok(())
}

/// Encode the label as the raster byte stream sent over TCP.
pub fn gen_tcp_data(td: &TapeDisplay) -> Result<Vec<u8>> {
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
    Ok(tcp_data)
}

/// Query the printer for the loaded tape.
pub fn detect_tape(device_ip: &str) -> Result<Tape> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("failed to bind")?;
    let info = StatusRequest::send(&socket, device_ip)?;
    if let PrinterStatus::SomeTape(t) = info {
        Ok(t)
    } else {
        Err(anyhow!("Failed to detect tape width. status: {:?}", info))
    }
}

/// Send the label to the printer and wait until printing finishes.
pub fn print_display(device_ip: &str, td: &TapeDisplay) -> Result<()> {
    print_tcp_data(device_ip, &gen_tcp_data(td)?)
}
