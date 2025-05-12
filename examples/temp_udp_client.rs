use std::net::UdpSocket;
use std::io;

fn main() -> io::Result<()> {
    let server_addr = "127.0.0.1:8088";
    let client_bind_addr = "127.0.0.1:0"; // Let OS choose a port

    // Bind the client socket to a local address
    let socket = UdpSocket::bind(client_bind_addr)?;
    println!("[UDP Client] Bound to local address: {}", socket.local_addr()?);

    // Connect to the server address (optional for UDP, but can be useful)
    // For send(), connect() specifies the default remote address.
    // For send_to(), it's not strictly necessary.
    // socket.connect(server_addr)?;,
    // println!("[UDP Client] Connected to server: {}", server_addr);

    // Construct a simplified QUIC Initial packet
    // First Byte: Type (Initial=00), Fixed Bit=1, Header Form=1, PN Len=0 (+1 = 1 byte) -> 11000000 = 0xC0
    // Version: 0x00000001 (QUIC v1)
    // DCID Len: 0x04
    // DCID: 0x01020304
    // SCID Len: 0x04
    // SCID: 0x05060708
    // Token Len: 0x00 (simplified, var-int would be just 0x00)
    // Token: (empty)
    // Length (PN + Payload): 0x05 (PN 1 byte, Payload 4 bytes - simplified var-int)
    // Packet Number: 0xAA (1 byte, raw)
    // Payload: 0xDEADC0DE (4 bytes)
    let message: Vec<u8> = vec![
        0xc0, // First Byte (Type=Initial, Fixed=1, Long=1, PN Len=1byte)
        0x00, 0x00, 0x00, 0x01, // Version (QUIC v1)
        0x04,       // DCID Len
        0x01, 0x02, 0x03, 0x04, // DCID
        0x04,       // SCID Len
        0x05, 0x06, 0x07, 0x08, // SCID
        0x00,       // Token Len (simplified to 0)
        // Token is empty
        0x05,       // Length (PN 1 byte + Payload 4 bytes = 5) (simplified)
        0xAA,       // Packet Number (1 byte, raw)
        0xDE, 0xAD, 0xC0, 0xDE // Payload (4 bytes)
    ];

    println!("[UDP Client] Sending QUIC Initial (simplified) packet: {:?}", message);
    println!("[UDP Client] Packet size: {} bytes", message.len());


    // Send data using send_to
    socket.send_to(&message, server_addr)?;
    println!("[UDP Client] Message sent.");

    // Optionally, try to receive a response (our current server doesn't send one)
    // Set a read timeout to avoid blocking indefinitely
    // socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    // let mut buf = [0; 1024];
    // match socket.recv_from(&mut buf) {
    //     Ok((number_of_bytes, src_addr)) => {
    //         println!("[UDP Client] Received {} bytes from {}: {}", number_of_bytes, src_addr, String::from_utf8_lossy(&buf[..number_of_bytes]));
    //     }
    //     Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
    //         println!("[UDP Client] No response received (as expected).");
    //     }
    //     Err(e) => {
    //         eprintln!("[UDP Client] Error receiving response: {}", e);
    //     }
    // }

    Ok(())
}