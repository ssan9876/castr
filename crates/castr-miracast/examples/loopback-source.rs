//! Drives a running sink from the source side over the loopback of the group
//! interface, with no radio and no Windows machine involved.
//!
//! It speaks the source half of the RTSP negotiation using the same fixtures
//! the unit tests use, then sends a synthetic transport stream to the sink's
//! RTP port. The point is to prove the socket layer: that the sink accepts a
//! connection, answers every message, reaches PLAY, and hands access units to
//! the decoder. The payload is synthetic, so the decoder is expected to reject
//! the units; what this test proves is that they arrive.
//!
//! Usage: `loopback-source [sink-address] [units]`, default
//! `192.168.173.1:7236` and 48 units.

use castr_miracast::test_support::{negotiation_to_playing, recorded_stream};
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    // loopback-source [addr] [units] [--drop N] [--vanish]
    let mut addr = "192.168.173.1:7236".to_string();
    let mut units: u32 = 48;
    let mut drop_percent: u32 = 0;
    let mut vanish = false;
    let mut positional = 0;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--drop" => {
                drop_percent = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "--vanish" => vanish = true,
            _ => {
                match positional {
                    0 => addr = a,
                    1 => units = a.parse().unwrap_or(48),
                    _ => {}
                }
                positional += 1;
            }
        }
    }
    let rtp_addr = {
        let host = addr.split(':').next().unwrap_or("192.168.173.1");
        format!("{host}:5000")
    };

    println!("connecting to {addr}");
    let mut rtsp = TcpStream::connect(&addr)?;
    rtsp.set_read_timeout(Some(Duration::from_millis(500)))?;
    rtsp.set_nodelay(true)?;

    for msg in negotiation_to_playing() {
        let first = msg.lines().next().unwrap_or("").to_string();
        rtsp.write_all(msg.as_bytes())?;
        println!("> {first}");
        // Give the sink a moment to answer, and print whatever it sends: its
        // replies and its own requests share this connection.
        std::thread::sleep(Duration::from_millis(200));
        let mut buf = [0u8; 8192];
        match rtsp.read(&mut buf) {
            Ok(0) => {
                println!("! the sink closed the connection");
                return Ok(());
            }
            Ok(n) => {
                for line in String::from_utf8_lossy(&buf[..n]).lines() {
                    if !line.is_empty() {
                        println!("< {line}");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }

    println!("sending {units} access units to {rtp_addr}");
    let udp = UdpSocket::bind("0.0.0.0:0")?;
    let stream = recorded_stream(units);
    let count = stream.len();
    let mut sent = 0u32;
    for (i, datagram) in stream.into_iter().enumerate() {
        // Drop `drop_percent` out of every hundred, spread through the
        // stream: sustained loss, not one contiguous hole, because a hole is a
        // different failure and the ladder is meant to see the former. Never
        // drop datagram 0: it carries the PAT/PMT, and without them the demux
        // never learns the stream PIDs, so nothing would flow at all.
        if drop_percent > 0 && i > 0 && (i as u32 % 100) < drop_percent {
            continue;
        }
        if vanish && i == count / 2 {
            // Stop sending and stop answering, without closing the socket: this
            // is what a radio going away looks like, as opposed to a clean
            // teardown the sink would see as a normal end.
            println!("vanishing after {sent} datagrams");
            std::thread::sleep(Duration::from_secs(45));
            return Ok(());
        }
        udp.send_to(&datagram, &rtp_addr)?;
        sent += 1;
        // Roughly a frame apart, so the sink's timing behaves as it would
        // with a real source rather than being flooded.
        std::thread::sleep(Duration::from_millis(33));
    }
    println!("sent {sent} datagrams");

    // Hold the connection open briefly so the sink's keep-alive and any IDR
    // request land in the log rather than racing the teardown.
    std::thread::sleep(Duration::from_secs(2));
    let mut buf = [0u8; 8192];
    if let Ok(n) = rtsp.read(&mut buf) {
        for line in String::from_utf8_lossy(&buf[..n]).lines() {
            if !line.is_empty() {
                println!("< {line}");
            }
        }
    }
    println!("done");
    Ok(())
}
