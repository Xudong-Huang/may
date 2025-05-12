// QUIC Server implementation for may-crate

use std::net::SocketAddr;
use std::io::{self, ErrorKind};
use std::thread;

use crate::net::UdpSocket;
use crate::go;
use crate::net::quic::packet::InitialPacket; // Added
use crate::net::quic::error::QuicError;   // Added

// Basic QuicServer structure
pub struct QuicServer {
    // We might add fields here later, like a config or a connection manager
}

impl QuicServer {
    pub fn new() -> Self {
        QuicServer {}
    }

    // Starts the QUIC server and listens for incoming UDP packets.
    pub fn start(self, addr: &str) -> io::Result<()> {
        println!("[QUIC Server] Starting on {}", addr);
        let socket = UdpSocket::bind(addr)?;
        println!("[QUIC Server] Bound to {}", socket.local_addr()?);

        let mut buf = vec![0u8; 2048]; // Initial buffer size, can be tuned

        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, src_addr)) => {
                    let packet_data = buf[..len].to_vec(); // Clone data before moving into coroutine
                    
                    go!(move || {
                        handle_packet(&packet_data, src_addr);
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // This should ideally not happen frequently with may's UdpSocket
                    // eprintln!("[QUIC Server] recv_from returned WouldBlock");
                    // may::coroutine::yield_now(); // Consider if needed
                    continue;
                }
                Err(e) => {
                    eprintln!("[QUIC Server] Error receiving packet: {}", e);
                    if e.kind() != ErrorKind::Interrupted {
                        return Err(e);
                    }
                }
            }
        }
        // Ok(()) // Loop is infinite
    }
}

// Packet handling logic
fn handle_packet(data: &[u8], src_addr: SocketAddr) {
    println!("[QUIC Handler {:?}] Received {} bytes from {}", 
             thread::current().id(), data.len(), src_addr);

    match InitialPacket::from_bytes(data) {
        Ok(initial_packet) => {
            println!("[QUIC Handler {:?}] Successfully parsed Initial Packet:", thread::current().id());
            println!("  Version: 0x{:08x}", initial_packet.version);
            println!("  DCID: {:?}", initial_packet.destination_connection_id);
            println!("  SCID: {:?}", initial_packet.source_connection_id);
            println!("  Token: {:?}", initial_packet.token);
            println!("  Length (PN+Payload): {}", initial_packet.length);
            println!("  Packet Number (raw): {}", initial_packet.packet_number);
            // Frames are now parsed
            println!("  Frames (count: {}):", initial_packet.frames.len());
            for (i, frame) in initial_packet.frames.iter().enumerate() {
                if i < 5 { // Print details for a few frames
                    println!("    Frame[{}]: {:?}", i, frame);
                } else if i == 5 {
                    println!("    ... (more frames truncated)");
                    break;
                }
            }
        }
        Err(e) => {
            eprintln!("[QUIC Handler {:?}] Failed to parse packet as Initial: {}", thread::current().id(), e);
            // For now, just log. Later, might try parsing as other packet types or send Version Negotiation.
            match e {
                QuicError::InvalidPacket(reason) if reason.contains("Header Form bit is not 1") 
                                                || reason.contains("Fixed Bit is not 1") 
                                                || reason.contains("Long Packet Type for Initial") => {
                    // These are strong indicators it's not a Long Header Initial packet.
                    // Could be a Short Header or a different Long Header type.
                    // Or simply a non-QUIC packet.
                    println!("[QUIC Handler {:?}] Packet does not appear to be a QUIC Initial Long Header packet.", thread::current().id());
                }
                _ => {
                    // Other parse errors
                }
            }
        }
    }
}

impl Default for QuicServer {
    fn default() -> Self {
        Self::new()
    }
}