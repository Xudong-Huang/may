// QUIC connection state management

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Instant;

// Assuming Packet and its variants (InitialPacket, ShortHeaderPacket, etc.) are public
use crate::net::quic::packet::{Packet, ConnectionId, InitialPacket, ShortHeaderPacket, VersionNegotiationPacket, RetryPacket};
use crate::net::quic::frame::Frame;
use crate::net::quic::error::QuicError; // Assuming a QuicError type exists

/// Represents the different packet number spaces used in QUIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketSpaceType {
    Initial,
    Handshake,
    ApplicationData, // For 0-RTT and 1-RTT data
}

/// Manages state for a single packet number space.
#[derive(Debug)]
pub struct PacketSpace {
    /// The type of this packet number space.
    space_type: PacketSpaceType,
    /// The next packet number to use for sending packets in this space.
    next_packet_number: u64,
    /// The largest packet number acknowledged by the peer in this space.
    largest_acked_packet_number: Option<u64>,
    /// Packets sent but not yet acknowledged.
    /// Stores (packet_number, sent_time, frames_in_packet_for_retransmission)
    // TODO: This needs a more robust structure for loss detection and retransmission.
    sent_packets: VecDeque<(u64, Instant, Vec<Frame>)>,
    // TODO: Add fields for ECN counts, ACK-eliciting packet tracking, etc.
}

impl PacketSpace {
    pub fn new(space_type: PacketSpaceType) -> Self {
        PacketSpace {
            space_type,
            next_packet_number: 0,
            largest_acked_packet_number: None,
            sent_packets: VecDeque::new(),
        }
    }

    pub fn get_next_packet_number(&mut self) -> u64 {
        let pn = self.next_packet_number;
        self.next_packet_number += 1;
        pn
    }

    // TODO: Methods for ack processing, loss detection, retransmission queueing etc.
}

/// Represents the state of a QUIC connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// The connection is in the process of a handshake.
    Handshaking,
    /// The connection is active and can send/receive data.
    Active,
    /// The connection is closing gracefully.
    Closing,
    /// The connection has been closed, possibly due to an error or timeout.
    Closed,
    /// The connection has been drained, all data acknowledged or timed out.
    Drained,
}

/// Represents a QUIC connection.
#[derive(Debug)]
pub struct Connection {
    /// The current state of the connection.
    state: ConnectionState,
    /// The address of the peer.
    peer_addr: SocketAddr,
    /// The local address for this connection.
    local_addr: SocketAddr,
    /// The Connection ID chosen by the local endpoint.
    source_cid: ConnectionId,
    /// The Connection ID chosen by the peer.
    destination_cid: ConnectionId,
    /// Packet number space for Initial packets.
    initial_space: PacketSpace,
    /// Packet number space for Handshake packets.
    handshake_space: PacketSpace,
    /// Packet number space for Application (0-RTT and 1-RTT) data.
    app_data_space: PacketSpace,
    // TODO: Add fields for stream management
    // TODO: Add fields for cryptographic state
    // TODO: Add fields for flow control
    // TODO: Add fields for congestion control
}

impl Connection {
    /// Creates a new QUIC connection (typically initiated by a client).
    pub fn new_client(
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        source_cid: ConnectionId,
        destination_cid: ConnectionId,
    ) -> Self {
        Connection {
            state: ConnectionState::Handshaking,
            peer_addr,
            local_addr,
            source_cid,
            destination_cid,
            initial_space: PacketSpace::new(PacketSpaceType::Initial),
            handshake_space: PacketSpace::new(PacketSpaceType::Handshake),
            app_data_space: PacketSpace::new(PacketSpaceType::ApplicationData),
        }
    }

    /// Creates a new QUIC connection (typically initiated by a server upon receiving an Initial packet).
    pub fn new_server(
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        source_cid: ConnectionId, // Server's chosen CID
        destination_cid: ConnectionId, // Client's initial destination CID (becomes server's source CID for replies)
    ) -> Self {
        Connection {
            state: ConnectionState::Handshaking,
            peer_addr,
            local_addr,
            source_cid,
            destination_cid,
            initial_space: PacketSpace::new(PacketSpaceType::Initial),
            handshake_space: PacketSpace::new(PacketSpaceType::Handshake),
            app_data_space: PacketSpace::new(PacketSpaceType::ApplicationData),
        }
    }

    /// Returns the current state of the connection.
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Returns a mutable reference to the specified packet number space.
    pub fn packet_space_mut(&mut self, space_type: PacketSpaceType) -> &mut PacketSpace {
        match space_type {
            PacketSpaceType::Initial => &mut self.initial_space,
            PacketSpaceType::Handshake => &mut self.handshake_space,
            PacketSpaceType::ApplicationData => &mut self.app_data_space,
        }
    }

    /// Handles an incoming datagram received from the network.
    pub fn handle_incoming_datagram(
        &mut self,
        datagram_payload: &[u8],
        _received_at: Instant, // Will be used for RTT, ack delays etc.
    ) -> Result<(), QuicError> {
        // Attempt to parse the packet.
        // The `from_bytes` function in `packet.rs` needs to handle differentiating
        // packet types (Initial, Handshake, 0-RTT, 1-RTT Short Header, Version Negotiation, Retry).
        // The second argument to from_bytes is the expected length of the Destination Connection ID
        // for Short Header packets, as this length is not encoded in the Short Header itself.
        let packet = Packet::from_bytes(datagram_payload, self.destination_cid.len())?;

        match packet {
            Packet::Initial(initial_packet) => {
                self.handle_initial_packet(initial_packet)?;
            }
            Packet::Short(short_packet_header, frames) => {
                self.handle_short_header_packet(short_packet_header, frames)?;
            }
            Packet::VersionNegotiation(vn_packet) => {
                self.handle_version_negotiation_packet(vn_packet)?;
            }
            Packet::Retry(retry_packet) => {
                self.handle_retry_packet(retry_packet)?;
            }
            // Packet::Handshake and Packet::ZeroRTT would be other variants.
            // These might be part of an enum like LongHeaderPacketType within InitialPacket,
            // or Packet::from_bytes might return a more specific enum.
            // For now, assuming packet.rs handles this differentiation.
            _ => {
                // Handle other packet types like Handshake, 0-RTT if they are separate variants
                // or log/error for unexpected packet types.
                // For example, if Handshake packets are not yet a direct variant of `Packet`:
                // if initial_packet.header.ptype == PacketType::Handshake { ... }
                println!("Received unhandled packet type for now.");
                // return Err(QuicError::UnsupportedPacketType); // Or similar
            }
        }
        Ok(())
    }

    fn handle_initial_packet(&mut self, packet: InitialPacket) -> Result<(), QuicError> {
        // TODO:
        // 1. Validate DCID matches one of the connection's CIDs (if server) or SCID (if client).
        //    For a server, packet.destination_connection_id should match one of its CIDs.
        //    For a client, packet.source_connection_id is what it expects the server to use as DCID for its replies.
        //    And packet.destination_connection_id should match what the client initially sent.
        // 2. Decrypt payload using Initial secrets.
        // 3. Process frames (likely CRYPTO and ACK frames).
        // 4. Update packet number space.
        println!("Handling Initial Packet: {:?}", packet);
        Ok(())
    }

    fn handle_short_header_packet(&mut self, header: ShortHeaderPacket, frames: Vec<Frame>) -> Result<(), QuicError> {
        // TODO:
        // 1. Validate DCID (header.destination_connection_id should match self.source_cid if client, or one of server's CIDs).
        // 2. Decrypt payload (frames were already "decrypted" if they came from Packet::Short, but header protection is separate).
        //    Actually, frames in Packet::Short are already decrypted. Packet number decryption and header protection are key.
        // 3. Process frames.
        // 4. Update packet number space (ApplicationData).
        println!("Handling Short Header Packet: {:?}, Frames: {:?}", header, frames);
        Ok(())
    }

    fn handle_version_negotiation_packet(&mut self, packet: VersionNegotiationPacket) -> Result<(), QuicError> {
        // TODO:
        // 1. Client-side: If this is a VN packet in response to client's Initial,
        //    check if any supported versions are offered. If so, select one and restart connection.
        //    If not, fail connection.
        // 2. Server-side: Servers do not receive VN packets. This would be an error.
        println!("Handling Version Negotiation Packet: {:?}", packet);
        if self.is_server() { // Assuming an `is_server()` method or similar role tracking
            return Err(QuicError::ProtocolViolation("Server received Version Negotiation packet".to_string()));
        }
        // Client logic to process VN
        self.state = ConnectionState::Closed; // Or a more specific error state
        Err(QuicError::VersionNegotiationError(packet.supported_versions))
    }

    fn handle_retry_packet(&mut self, packet: RetryPacket) -> Result<(), QuicError> {
        // TODO:
        // 1. Client-side: Validate retry token, update DCID, re-send Initial with new token.
        // 2. Server-side: Servers do not receive Retry packets.
        println!("Handling Retry Packet: {:?}", packet);
         if self.is_server() {
            return Err(QuicError::ProtocolViolation("Server received Retry packet".to_string()));
        }
        // Client logic to process Retry
        // Example: self.destination_cid = packet.header.source_connection_id;
        //          self.retry_token = Some(packet.retry_token);
        //          Re-initiate Initial packet sending.
        self.state = ConnectionState::Handshaking; // Or a more specific state to re-initiate
        Err(QuicError::RetryReceived("Client needs to handle Retry packet".to_string()))
    }

    // Helper to determine role, placeholder for now
    fn is_server(&self) -> bool {
        // This needs a proper way to determine if the connection is acting as a server.
        // For now, let's assume a client always initiates with a non-empty destination_cid
        // that it expects the server to use.
        // This is a simplification and might not hold true for all scenarios (e.g. server initiated CIDs).
        // A more robust way would be to store the role explicitly.
        true // Placeholder, needs proper role management
    }


    // TODO: Implement methods for:
    // - Sending outgoing packets
    // - Managing streams
    // - Handling timers (e.g., idle timeout, retransmission timeout)
    // - Advancing the connection state
    // - Closing the connection
}