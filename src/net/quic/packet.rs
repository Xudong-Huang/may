// Defines QUIC packet structures and parsing logic.

use super::error::QuicError;
use super::frame::Frame;

// Helper functions for variable-length integer encoding/decoding
// RFC 9000 Section 16

/// Decodes a variable-length integer from the provided byte slice.
///
/// The slice `data` is advanced by the number of bytes read.
/// Returns the decoded u64 value and the number of bytes consumed.
pub fn decode_varint(data: &mut &[u8]) -> Result<(u64, usize), QuicError> {
    if data.is_empty() {
        return Err(QuicError::ParseError("VarInt: Data is empty".to_string()));
    }
    let first_byte = data[0];
    let length_prefix = first_byte >> 6;
    
    match length_prefix {
        0b00 => { // 1-byte
            if data.len() < 1 { return Err(QuicError::ParseError("VarInt: Not enough data for 1-byte encoding".to_string())); }
            let val = (data[0] & 0x3F) as u64;
            *data = &data[1..];
            Ok((val, 1))
        }
        0b01 => { // 2-byte
            if data.len() < 2 { return Err(QuicError::ParseError("VarInt: Not enough data for 2-byte encoding".to_string())); }
            let val = u16::from_be_bytes([data[0] & 0x3F, data[1]]) as u64;
            *data = &data[2..];
            Ok((val, 2))
        }
        0b10 => { // 4-byte
            if data.len() < 4 { return Err(QuicError::ParseError("VarInt: Not enough data for 4-byte encoding".to_string())); }
            let val = u32::from_be_bytes([data[0] & 0x3F, data[1], data[2], data[3]]) as u64;
            *data = &data[4..];
            Ok((val, 4))
        }
        0b11 => { // 8-byte
            if data.len() < 8 { return Err(QuicError::ParseError("VarInt: Not enough data for 8-byte encoding".to_string())); }
            let val = ((data[0] & 0x3F) as u64) << 56
                    | (data[1] as u64) << 48
                    | (data[2] as u64) << 40
                    | (data[3] as u64) << 32
                    | (data[4] as u64) << 24
                    | (data[5] as u64) << 16
                    | (data[6] as u64) << 8
                    | (data[7] as u64);
            *data = &data[8..];
            Ok((val, 8))
        }
        _ => unreachable!("Invalid VarInt length prefix due to 2-bit mask"),
    }
}

/// Encodes a u64 value into a variable-length integer byte vector.
/// Panics if the value is too large to be represented ( > 2^62 - 1).
pub fn encode_varint(value: u64) -> Vec<u8> {
    const MAX_VARINT_VALUE: u64 = (1 << 62) - 1;
    if value > MAX_VARINT_VALUE {
        panic!("Value {} too large for QUIC VarInt encoding (max is {})", value, MAX_VARINT_VALUE);
    }

    if value <= 0x3F { 
        vec![value as u8]
    } else if value <= 0x3FFF { 
        let mut buf = (value as u16).to_be_bytes();
        buf[0] = (buf[0] & 0x3F) | 0x40; 
        buf.to_vec()
    } else if value <= 0x3FFFFFFF { 
        let mut buf = (value as u32).to_be_bytes(); 
        buf[0] = (buf[0] & 0x3F) | 0x80; 
        buf.to_vec()
    } else { 
        let mut buf = value.to_be_bytes(); 
        buf[0] = (buf[0] & 0x3F) | 0xC0; 
        buf.to_vec()
    }
}

/// Represents a QUIC Initial packet.
/// RFC 9000 Section 17.2.2
#[derive(Debug)]
pub struct InitialPacket {
    pub version: u32,
    pub destination_connection_id: Vec<u8>,
    pub source_connection_id: Vec<u8>,
    pub token: Vec<u8>,
    pub length: u64, 
    pub packet_number: u32, 
    pub frames: Vec<Frame>,
}

impl InitialPacket {
    pub fn from_bytes(data: &[u8]) -> Result<Self, QuicError> {
        if data.len() < 6 { 
            return Err(QuicError::InvalidPacket("Data too short for Initial packet header basics".to_string()));
        }
        let mut remaining_data = data;
        let first_byte = remaining_data[0];
        if (first_byte & 0b1000_0000) == 0 {
            return Err(QuicError::InvalidPacket("Invalid Initial packet: Header Form bit is not 1".to_string()));
        }
        if (first_byte & 0b0100_0000) == 0 {
            return Err(QuicError::InvalidPacket("Invalid Initial packet: Fixed Bit is not 1".to_string()));
        }
        let long_packet_type = (first_byte & 0b0011_0000) >> 4;
        if long_packet_type != 0b00 { 
            return Err(QuicError::InvalidPacket(format!("Invalid Long Packet Type for Initial: {:02x}", long_packet_type)));
        }
        if (first_byte & 0b0000_1100) != 0 {
            return Err(QuicError::InvalidPacket("Invalid Initial packet: Reserved bits are not 0".to_string()));
        }
        let pn_length_bits = first_byte & 0b0000_0011;
        let actual_pn_len = (pn_length_bits + 1) as usize;
        remaining_data = &remaining_data[1..]; 
        if remaining_data.len() < 4 { return Err(QuicError::ParseError("Data too short for Version".to_string()));}
        let version = u32::from_be_bytes([remaining_data[0], remaining_data[1], remaining_data[2], remaining_data[3]]);
        remaining_data = &remaining_data[4..];
        if remaining_data.is_empty() { return Err(QuicError::ParseError("Data too short for DCID Length".to_string())); }
        let dcid_len = remaining_data[0] as usize;
        remaining_data = &remaining_data[1..];
        if dcid_len > 20 { return Err(QuicError::InvalidPacket("DCID length exceeds 20 bytes".to_string())); }
        if remaining_data.len() < dcid_len { return Err(QuicError::ParseError("Data too short for DCID".to_string())); }
        let destination_connection_id = remaining_data[..dcid_len].to_vec();
        remaining_data = &remaining_data[dcid_len..];
        if remaining_data.is_empty() { return Err(QuicError::ParseError("Data too short for SCID Length".to_string())); }
        let scid_len = remaining_data[0] as usize;
        remaining_data = &remaining_data[1..];
        if scid_len > 20 { return Err(QuicError::InvalidPacket("SCID length exceeds 20 bytes".to_string())); }
        if remaining_data.len() < scid_len { return Err(QuicError::ParseError("Data too short for SCID".to_string())); }
        let source_connection_id = remaining_data[..scid_len].to_vec();
        remaining_data = &remaining_data[scid_len..];
        let (token_len_val, _consumed) = decode_varint(&mut remaining_data)
            .map_err(|e| QuicError::ParseError(format!("Failed to decode Token Length: {}", e)))?;
        let token_len = token_len_val as usize;
        if remaining_data.len() < token_len { return Err(QuicError::ParseError("Data too short for Token".to_string())); }
        let token = remaining_data[..token_len].to_vec();
        remaining_data = &remaining_data[token_len..];
        let (length_val, _consumed_len) = decode_varint(&mut remaining_data)
            .map_err(|e| QuicError::ParseError(format!("Failed to decode Length: {}", e)))?;
        if length_val > remaining_data.len() as u64 {
            return Err(QuicError::InvalidPacket(format!("Length field ({}) exceeds remaining packet data ({})", length_val, remaining_data.len())));
        }
        if length_val < actual_pn_len as u64 { 
            return Err(QuicError::InvalidPacket("Length field is too small for the specified Packet Number Length".to_string()));
        }
        if remaining_data.len() < actual_pn_len {
             return Err(QuicError::ParseError("Data too short for Packet Number".to_string()));
        }
        let mut packet_number_bytes = [0u8; 4];
        packet_number_bytes[4-actual_pn_len..].copy_from_slice(&remaining_data[..actual_pn_len]);
        let packet_number = u32::from_be_bytes(packet_number_bytes);
        remaining_data = &remaining_data[actual_pn_len..];
        let frames_bytes_len = length_val as usize - actual_pn_len;
        if remaining_data.len() < frames_bytes_len {
            return Err(QuicError::ParseError(
                "Data too short for Frames based on Length field".to_string(),
            ));
        }
        let mut frames_data_slice = &remaining_data[..frames_bytes_len];
        let mut frames = Vec::new();
        while !frames_data_slice.is_empty() {
            match Frame::from_bytes(frames_data_slice) {
                Ok((frame, consumed)) => {
                    frames.push(frame);
                    if consumed == 0 { 
                        return Err(QuicError::ParseError(
                            "Frame parser consumed 0 bytes, this would lead to an infinite loop.".to_string()
                        ));
                    }
                    if consumed > frames_data_slice.len() {
                         return Err(QuicError::ParseError(format!(
                            "Frame::from_bytes reported consumed {} bytes but only {} bytes were available",
                            consumed,
                            frames_data_slice.len()
                        )));
                    }
                    frames_data_slice = &frames_data_slice[consumed..];
                }
                Err(e) => {
                    return Err(QuicError::ParseError(format!(
                        "Failed to parse frame: {:?}. Partially parsed frames: {:?}. Remaining data: {:?}",
                        e, frames, frames_data_slice
                    )));
                }
            }
        }
        Ok(InitialPacket {
            version,
            destination_connection_id,
            source_connection_id,
            token,
            length: length_val, 
            packet_number,
            frames,
        })
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, QuicError> {
        let mut frames_bytes = Vec::new();
        for frame in &self.frames {
            frames_bytes.extend_from_slice(&frame.to_bytes());
        }
        let pn_val = self.packet_number;
        let actual_pn_len = if pn_val <= 0xFF {
            1
        } else if pn_val <= 0xFFFF {
            2
        } else if pn_val <= 0xFF_FFFF {
            3
        } else {
            4
        };
        let packet_number_all_bytes = self.packet_number.to_be_bytes();
        let packet_number_actual_bytes = &packet_number_all_bytes[(4 - actual_pn_len)..];
        let length_field_value = (actual_pn_len + frames_bytes.len()) as u64;
        let length_field_encoded_bytes = encode_varint(length_field_value);
        if self.destination_connection_id.len() > 20 {
            return Err(QuicError::InvalidPacket("DCID too long for InitialPacket".to_string()));
        }
        if self.source_connection_id.len() > 20 {
            return Err(QuicError::InvalidPacket("SCID too long for InitialPacket".to_string()));
        }
        let dcid_len_byte = self.destination_connection_id.len() as u8;
        let scid_len_byte = self.source_connection_id.len() as u8;
        let token_len_encoded_bytes = encode_varint(self.token.len() as u64);
        let mut first_byte: u8 = 0b1100_0000; 
        first_byte |= (actual_pn_len - 1) as u8; 
        let mut bytes = Vec::new();
        bytes.push(first_byte);
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.push(dcid_len_byte);
        bytes.extend_from_slice(&self.destination_connection_id);
        bytes.push(scid_len_byte);
        bytes.extend_from_slice(&self.source_connection_id);
        bytes.extend_from_slice(&token_len_encoded_bytes);
        bytes.extend_from_slice(&self.token);
        bytes.extend_from_slice(&length_field_encoded_bytes);
        bytes.extend_from_slice(packet_number_actual_bytes);
        bytes.extend_from_slice(&frames_bytes);
        
        Ok(bytes)
    }
}

/// Represents a QUIC Version Negotiation packet.
/// RFC 9000 Section 17.2.1
#[derive(Debug, PartialEq, Eq)]
pub struct VersionNegotiationPacket {
    pub destination_connection_id: Vec<u8>,
    pub source_connection_id: Vec<u8>,
    pub supported_versions: Vec<u32>,
}

impl VersionNegotiationPacket {
    pub fn to_bytes(&self) -> Result<Vec<u8>, QuicError> {
        if self.destination_connection_id.len() > 255 { 
            return Err(QuicError::InvalidPacket("VN Packet: DCID too long".to_string()));
        }
        if self.source_connection_id.len() > 255 {
            return Err(QuicError::InvalidPacket("VN Packet: SCID too long".to_string()));
        }
        if self.supported_versions.is_empty() {
            return Err(QuicError::InvalidPacket("VN Packet: Supported versions list cannot be empty".to_string()));
        }
        let mut bytes = Vec::new();
        let first_byte: u8 = 0x80 | 0x0A; 
        bytes.push(first_byte);
        bytes.extend_from_slice(&0x00000000u32.to_be_bytes()); 
        bytes.push(self.destination_connection_id.len() as u8);
        bytes.extend_from_slice(&self.destination_connection_id);
        bytes.push(self.source_connection_id.len() as u8);
        bytes.extend_from_slice(&self.source_connection_id);
        for &version in &self.supported_versions {
            bytes.extend_from_slice(&version.to_be_bytes());
        }
        Ok(bytes)
    }
}

/// Represents a QUIC Retry packet.
/// RFC 9000 Section 17.2.5
#[derive(Debug, PartialEq, Eq)]
pub struct RetryPacket {
    pub version: u32,
    pub destination_connection_id: Vec<u8>,
    pub source_connection_id: Vec<u8>,
    pub retry_token: Vec<u8>,
    pub retry_integrity_tag: [u8; 16],
}

impl RetryPacket {
    pub fn from_bytes(data: &[u8], _original_client_dcid: &[u8]) -> Result<Self, QuicError> { 
        if data.len() < (1 + 4 + 1 + 0 + 1 + 0 + 1 + 16) { 
            return Err(QuicError::InvalidPacket("Retry Packet: Data too short for minimal structure".to_string()));
        }
        let first_byte = data[0];
        if (first_byte & 0b1111_0000) != 0b1111_0000 { 
             return Err(QuicError::InvalidPacket(format!("Retry Packet: Invalid first byte (expected 1111xxxx, got {:02x})", first_byte)));
        }
        let mut current_offset = 1;
        if data.len() < current_offset + 4 { return Err(QuicError::ParseError("Retry Packet: Data too short for Version".to_string())); }
        let version = u32::from_be_bytes(data[current_offset..current_offset+4].try_into().unwrap());
        current_offset += 4;
        if data.len() < current_offset + 1 { return Err(QuicError::ParseError("Retry Packet: Data too short for DCID Length".to_string())); }
        let dcid_len = data[current_offset] as usize;
        current_offset += 1;
        if dcid_len > 20 { return Err(QuicError::InvalidPacket("Retry Packet: DCID length exceeds 20".to_string())); }
        if data.len() < current_offset + dcid_len { return Err(QuicError::ParseError("Retry Packet: Data too short for DCID".to_string())); }
        let destination_connection_id = data[current_offset..current_offset + dcid_len].to_vec();
        current_offset += dcid_len;
        if data.len() < current_offset + 1 { return Err(QuicError::ParseError("Retry Packet: Data too short for SCID Length".to_string())); }
        let scid_len = data[current_offset] as usize;
        current_offset += 1;
        if scid_len > 20 { return Err(QuicError::InvalidPacket("Retry Packet: SCID length exceeds 20".to_string())); }
        if data.len() < current_offset + scid_len { return Err(QuicError::ParseError("Retry Packet: Data too short for SCID".to_string())); }
        let source_connection_id = data[current_offset..current_offset + scid_len].to_vec();
        current_offset += scid_len;
        if data.len() < current_offset + 16 + 1 { 
            return Err(QuicError::ParseError("Retry Packet: Data too short for Retry Token and Integrity Tag".to_string()));
        }
        let token_end_offset = data.len() - 16;
        if current_offset >= token_end_offset { 
             return Err(QuicError::InvalidPacket("Retry Packet: Retry Token cannot be empty (or offset error)".to_string()));
        }
        let retry_token = data[current_offset..token_end_offset].to_vec();
        let mut retry_integrity_tag = [0u8; 16];
        retry_integrity_tag.copy_from_slice(&data[token_end_offset..token_end_offset + 16]);
        Ok(RetryPacket {
            version,
            destination_connection_id,
            source_connection_id,
            retry_token,
            retry_integrity_tag,
        })
    }

    pub fn to_bytes(&self, _original_client_dcid: &[u8]) -> Result<Vec<u8>, QuicError> {
        if self.destination_connection_id.len() > 20 {
            return Err(QuicError::InvalidPacket("Retry Packet to_bytes: DCID too long".to_string()));
        }
        if self.source_connection_id.len() > 20 { 
            return Err(QuicError::InvalidPacket("Retry Packet to_bytes: SCID too long".to_string()));
        }
        if self.retry_token.is_empty() {
            return Err(QuicError::InvalidPacket("Retry Packet to_bytes: Retry Token cannot be empty".to_string()));
        }
        let mut bytes = Vec::new();
        let first_byte: u8 = 0b1111_0000 | 0x0A ; 
        bytes.push(first_byte);
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.push(self.destination_connection_id.len() as u8);
        bytes.extend_from_slice(&self.destination_connection_id);
        bytes.push(self.source_connection_id.len() as u8);
        bytes.extend_from_slice(&self.source_connection_id);
        bytes.extend_from_slice(&self.retry_token);
        bytes.extend_from_slice(&self.retry_integrity_tag); 
        Ok(bytes)
    }
}

/// Represents a QUIC Short Header packet.
/// RFC 9000 Section 17.3
#[derive(Debug, PartialEq, Eq)]
pub struct ShortHeaderPacket {
    pub spin_bit: bool,
    pub key_phase_bit: bool,
    pub destination_connection_id: Vec<u8>, 
    pub packet_number: u32, 
}

impl ShortHeaderPacket {
    pub fn from_bytes<'a>(data: &'a [u8], dcid_len: usize) -> Result<(Self, &'a [u8]), QuicError> {
        if data.is_empty() {
            return Err(QuicError::ParseError("Short Header: Data is empty".to_string()));
        }
        let first_byte = data[0];
        if (first_byte & 0b1100_0000) != 0b0100_0000 { 
            return Err(QuicError::InvalidPacket(
                format!("Short Header: Invalid Header Form or Fixed Bit (expected 01xxxxxx, got {:02x})", first_byte & 0b1100_0000)
            ));
        }
        if (first_byte & 0b0000_1100) != 0b0000_0000 { 
            return Err(QuicError::InvalidPacket(
                format!("Short Header: Reserved bits must be zero (got {:02x})", first_byte & 0b0000_1100)
            ));
        }
        let spin_bit = (first_byte & 0b0010_0000) != 0;    
        let key_phase_bit = (first_byte & 0b0001_0000) != 0; 
        let pn_len_encoded = first_byte & 0b0000_0011;     
        let actual_pn_len = (pn_len_encoded + 1) as usize; 
        let mut remaining_data = &data[1..]; 
        if remaining_data.len() < dcid_len {
            return Err(QuicError::ParseError(format!(
                "Short Header: Data too short for DCID (expected {}, got {})",
                dcid_len,
                remaining_data.len()
            )));
        }
        let destination_connection_id = remaining_data[..dcid_len].to_vec();
        remaining_data = &remaining_data[dcid_len..];
        if remaining_data.len() < actual_pn_len {
            return Err(QuicError::ParseError(format!(
                "Short Header: Data too short for Packet Number (expected {} bytes, got {} remaining)",
                actual_pn_len,
                remaining_data.len()
            )));
        }
        let mut pn_as_u32_bytes = [0u8; 4]; 
        pn_as_u32_bytes[(4 - actual_pn_len)..].copy_from_slice(&remaining_data[..actual_pn_len]);
        let packet_number = u32::from_be_bytes(pn_as_u32_bytes);
        remaining_data = &remaining_data[actual_pn_len..]; 
        Ok((
            ShortHeaderPacket {
                spin_bit,
                key_phase_bit,
                destination_connection_id,
                packet_number,
            },
            remaining_data, 
        ))
    }

    pub fn to_bytes(&self, frames: &[Frame]) -> Result<Vec<u8>, QuicError> {
        let mut frames_bytes = Vec::new();
        for frame in frames {
            frames_bytes.extend_from_slice(&frame.to_bytes());
        }

        let pn_val = self.packet_number;
        let (actual_pn_len, pn_len_bits_encoded) = if pn_val <= 0xFF {
            (1, 0b00)
        } else if pn_val <= 0xFFFF {
            (2, 0b01)
        } else if pn_val <= 0xFF_FFFF {
            (3, 0b10)
        } else {
            (4, 0b11)
        };
        let mut first_byte: u8 = 0b0100_0000; 
        if self.spin_bit {
            first_byte |= 0b0010_0000;
        }
        if self.key_phase_bit {
            first_byte |= 0b0001_0000;
        }
        first_byte |= pn_len_bits_encoded;
        
        let dcid_slice = &self.destination_connection_id;
        let mut bytes = Vec::with_capacity(1 + dcid_slice.len() + actual_pn_len + frames_bytes.len());
        
        bytes.push(first_byte);
        bytes.extend_from_slice(dcid_slice);
        
        let pn_all_bytes = self.packet_number.to_be_bytes();
        bytes.extend_from_slice(&pn_all_bytes[(4 - actual_pn_len)..]);
        
        bytes.extend_from_slice(&frames_bytes);
        
        Ok(bytes)
    }
}

/// Represents any QUIC packet.
#[derive(Debug)]
pub enum Packet {
    Initial(InitialPacket),
    VersionNegotiation(VersionNegotiationPacket),
    Retry(RetryPacket),
    Short(ShortHeaderPacket, Vec<Frame>),
    // TODO: Add other packet types: ZeroRTT, Handshake
}
impl Packet {
    // conn_dcid_len is required for parsing Short Header packets, as DCID length is not in the packet itself.
    pub fn from_bytes(data: &[u8], conn_dcid_len: usize) -> Result<Self, QuicError> {
        if data.is_empty() {
            return Err(QuicError::ParseError("Packet data is empty".to_string()));
        }

        let first_byte = data[0];
        let header_form = (first_byte & 0b1000_0000) >> 7;

        if header_form == 1 { // Long Header Packet
            if data.len() < 5 { 
                return Err(QuicError::InvalidPacket("Data too short for Long Header (minimum 5 bytes for type and version)".to_string()));
            }
            let long_packet_type = (first_byte & 0b0011_0000) >> 4;
            let version = u32::from_be_bytes(match data[1..5].try_into() {
                Ok(slice) => slice,
                Err(_) => return Err(QuicError::ParseError("Could not read version from long header".to_string())),
            });

            if version == 0x00000000 { 
                return Err(QuicError::InvalidPacket("Version Negotiation Packet (version 0x00000000) parsing not yet implemented in Packet::from_bytes".to_string()));
            }

            match long_packet_type {
                0b00 => { 
                    InitialPacket::from_bytes(data).map(Packet::Initial)
                }
                0b01 => { 
                    Err(QuicError::InvalidPacket("0-RTT Packet not supported".to_string()))
                }
                0b10 => { 
                    Err(QuicError::InvalidPacket("Handshake Packet not supported".to_string()))
                }
                0b11 => { 
                    Err(QuicError::InvalidPacket("Retry Packet parsing requires original_client_dcid, not supported directly in Packet::from_bytes".to_string()))
                }
                _ => unreachable!("Invalid Long Packet Type due to 2-bit mask"),
            }
        } else { // Short Header Packet (header_form == 0)
            let (short_header, frames_payload_slice) = ShortHeaderPacket::from_bytes(data, conn_dcid_len)?;
            
            let mut parsed_frames = Vec::new();
            let mut current_frames_payload = frames_payload_slice;

            while !current_frames_payload.is_empty() {
                match Frame::from_bytes(current_frames_payload) {
                    Ok((frame, consumed)) => {
                        parsed_frames.push(frame);
                        if consumed == 0 {
                            return Err(QuicError::ParseError(
                                "Frame parser consumed 0 bytes in Short Packet, would loop infinitely.".to_string()
                            ));
                        }
                        if consumed > current_frames_payload.len() {
                             return Err(QuicError::ParseError(format!(
                                "Frame parser reported consumed {} bytes but only {} were available in Short Packet payload",
                                consumed,
                                current_frames_payload.len()
                            )));
                        }
                        current_frames_payload = &current_frames_payload[consumed..];
                    }
                    Err(e) => {
                        return Err(QuicError::ParseError(format!(
                            "Failed to parse frame in Short Packet: {:?}. Partially parsed frames: {:?}. Remaining data: {:?}",
                            e, parsed_frames, current_frames_payload
                        )));
                    }
                }
            }
            Ok(Packet::Short(short_header, parsed_frames))
        }
    }
}

#[cfg(test)]
mod varint_tests { 
    use super::*;
    use crate::net::quic::error::QuicError; 
    #[test]
    fn test_decode_varint_examples() {
        let mut data1: &[u8] = &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c];
        let (val1, len1) = decode_varint(&mut data1).unwrap();
        assert_eq!(val1, 151_288_809_941_952_652);
        assert_eq!(len1, 8);
        assert!(data1.is_empty());

        let mut data2: &[u8] = &[0x9d, 0x7f, 0x3e, 0x7d];
        let (val2, len2) = decode_varint(&mut data2).unwrap();
        assert_eq!(val2, 494_878_333);
        assert_eq!(len2, 4);
        assert!(data2.is_empty());

        let mut data3: &[u8] = &[0x7b, 0xbd];
        let (val3, len3) = decode_varint(&mut data3).unwrap();
        assert_eq!(val3, 15293);
        assert_eq!(len3, 2);
        assert!(data3.is_empty());

        let mut data4: &[u8] = &[0x25];
        let (val4, len4) = decode_varint(&mut data4).unwrap();
        assert_eq!(val4, 37);
        assert_eq!(len4, 1);
        assert!(data4.is_empty());

        let mut data5: &[u8] = &[0x7b, 0xbd, 0xaa, 0xbb];
        let (val5, len5) = decode_varint(&mut data5).unwrap();
        assert_eq!(val5, 15293);
        assert_eq!(len5, 2);
        assert_eq!(data5, &[0xaa, 0xbb]);
    }

    #[test]
    fn test_encode_varint_examples() {
        assert_eq!(encode_varint(151_288_809_941_952_652), vec![0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c]);
        assert_eq!(encode_varint(494_878_333), vec![0x9d, 0x7f, 0x3e, 0x7d]);
        assert_eq!(encode_varint(15293), vec![0x7b, 0xbd]);
        assert_eq!(encode_varint(37), vec![0x25]);
        assert_eq!(encode_varint(0), vec![0x00]);
        assert_eq!(encode_varint(63), vec![0x3f]);
        assert_eq!(encode_varint(64), vec![0x40, 0x40]);
        assert_eq!(encode_varint(16383), vec![0x7f, 0xff]);
        assert_eq!(encode_varint(16384), vec![0x80, 0x00, 0x40, 0x00]);
        assert_eq!(encode_varint(1_073_741_823), vec![0xbf, 0xff, 0xff, 0xff]);
        assert_eq!(encode_varint(1_073_741_824), vec![0xc0, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00]);
        assert_eq!(encode_varint((1 << 62) - 1), vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    #[should_panic(expected = "Value 4611686018427387904 too large for QUIC VarInt encoding (max is 4611686018427387903)")]
    fn test_encode_varint_too_large() {
        encode_varint(1 << 62); 
    }

    #[test]
    fn test_decode_varint_insufficient_data() {
        let mut data_empty: &[u8] = &[];
        assert!(decode_varint(&mut data_empty).is_err());

        let mut data_short_2byte: &[u8] = &[0x40]; 
        assert!(decode_varint(&mut data_short_2byte).is_err());

        let mut data_short_4byte: &[u8] = &[0x80, 0x00, 0x00]; 
        assert!(decode_varint(&mut data_short_4byte).is_err());

        let mut data_short_8byte: &[u8] = &[0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; 
        assert!(decode_varint(&mut data_short_8byte).is_err());
    }

     #[test]
    fn test_decode_varint_error_variants() {
        let mut data: &[u8] = &[];
        match decode_varint(&mut data) {
            Err(QuicError::ParseError(msg)) => assert_eq!(msg, "VarInt: Data is empty"),
            res => panic!("Expected ParseError, got {:?}", res),
        }

        let mut data2: &[u8] = &[0b0100_0000]; 
        match decode_varint(&mut data2) {
            Err(QuicError::ParseError(msg)) => assert_eq!(msg, "VarInt: Not enough data for 2-byte encoding"),
            res => panic!("Expected ParseError, got {:?}", res),
        }
    }
}

#[cfg(test)]
mod vn_packet_tests {
    use super::*;

    #[test]
    fn test_version_negotiation_packet_to_bytes_basic() {
        let packet = VersionNegotiationPacket {
            destination_connection_id: vec![0x01, 0x02, 0x03, 0x04],
            source_connection_id: vec![0x05, 0x06, 0x07, 0x08, 0x09],
            supported_versions: vec![0x00000001, 0xff00001d],
        };
        let bytes = packet.to_bytes().unwrap();

        assert_eq!(bytes[0], 0x8A); 
        assert_eq!(&bytes[1..5], &0x00000000u32.to_be_bytes()); 
        assert_eq!(bytes[5], 4); 
        assert_eq!(&bytes[6..10], &[0x01, 0x02, 0x03, 0x04]); 
        assert_eq!(bytes[10], 5); 
        assert_eq!(&bytes[11..16], &[0x05, 0x06, 0x07, 0x08, 0x09]); 
        assert_eq!(&bytes[16..20], &0x00000001u32.to_be_bytes()); 
        assert_eq!(&bytes[20..24], &0xff00001du32.to_be_bytes()); 
        assert_eq!(bytes.len(), 1 + 4 + 1 + 4 + 1 + 5 + 4 + 4);
    }

    #[test]
    fn test_version_negotiation_packet_empty_dcid_scid() {
        let packet = VersionNegotiationPacket {
            destination_connection_id: vec![],
            source_connection_id: vec![],
            supported_versions: vec![0x01],
        };
        let bytes = packet.to_bytes().unwrap();
        assert_eq!(bytes[0], 0x8A); 
        assert_eq!(&bytes[1..5], &0x00000000u32.to_be_bytes());
        assert_eq!(bytes[5], 0); 
        assert_eq!(bytes[6], 0); 
        assert_eq!(&bytes[7..11], &0x00000001u32.to_be_bytes());
        assert_eq!(bytes.len(), 1 + 4 + 1 + 0 + 1 + 0 + 4);
    }

    #[test]
    fn test_version_negotiation_packet_max_cid_len() {
        let dcid = vec![0xaa; 20];
        let scid = vec![0xbb; 20];
        let packet = VersionNegotiationPacket {
            destination_connection_id: dcid.clone(),
            source_connection_id: scid.clone(),
            supported_versions: vec![0x01],
        };
        let bytes = packet.to_bytes().unwrap();
        assert_eq!(bytes[5], 20);
        assert_eq!(&bytes[6..26], dcid.as_slice());
        assert_eq!(bytes[26], 20);
        assert_eq!(&bytes[27..47], scid.as_slice());
    }

    #[test]
    fn test_vn_packet_error_on_empty_supported_versions() {
        let packet = VersionNegotiationPacket {
            destination_connection_id: vec![1],
            source_connection_id: vec![2],
            supported_versions: vec![],
        };
        assert!(packet.to_bytes().is_err());
    }

    #[test]
    fn test_vn_packet_error_on_too_long_cid() {
        let long_cid = vec![0u8; 256];
        let packet1 = VersionNegotiationPacket {
            destination_connection_id: long_cid.clone(),
            source_connection_id: vec![1],
            supported_versions: vec![1],
        };
         assert!(packet1.to_bytes().is_err());

        let packet2 = VersionNegotiationPacket {
            destination_connection_id: vec![1],
            source_connection_id: long_cid,
            supported_versions: vec![1],
        };
        assert!(packet2.to_bytes().is_err());
    }
}

#[cfg(test)]
mod retry_packet_tests {
    use super::*;
    const MOCK_ORIGINAL_CLIENT_DCID: &[u8] = &[0x0a, 0x0b, 0x0c, 0x0d];
    const MOCK_RETRY_TAG: [u8; 16] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f];

    #[test]
    fn test_retry_packet_to_bytes_and_from_bytes_basic() {
        let packet = RetryPacket {
            version: 0x00000001,
            destination_connection_id: vec![0x11, 0x22], 
            source_connection_id: vec![0x33, 0x44, 0x55], 
            retry_token: vec![0xaa, 0xbb, 0xcc],
            retry_integrity_tag: MOCK_RETRY_TAG,
        };
        let bytes = packet.to_bytes(MOCK_ORIGINAL_CLIENT_DCID).unwrap();

        assert_eq!(bytes[0], 0xFA); 
        assert_eq!(bytes[1..5], 0x00000001u32.to_be_bytes());
        assert_eq!(bytes[5], 2); 
        assert_eq!(bytes[6..8], [0x11, 0x22]);
        assert_eq!(bytes[8], 3); 
        assert_eq!(bytes[9..12], [0x33, 0x44, 0x55]);
        assert_eq!(bytes[12..15], [0xaa, 0xbb, 0xcc]);
        assert_eq!(bytes[15..31], MOCK_RETRY_TAG); 
        assert_eq!(bytes.len(), 1+4+1+2+1+3+3+16);

        let parsed_packet = RetryPacket::from_bytes(&bytes, MOCK_ORIGINAL_CLIENT_DCID).unwrap();
        assert_eq!(parsed_packet, packet);
    }

    #[test]
    fn test_retry_from_bytes_invalid_type() {
        let mut data = vec![0xE0]; 
        data.extend_from_slice(&0x00000001u32.to_be_bytes()); 
        data.push(0); 
        data.push(0); 
        data.extend_from_slice(&[0x01]); 
        data.extend_from_slice(&MOCK_RETRY_TAG);
        let result = RetryPacket::from_bytes(&data, MOCK_ORIGINAL_CLIENT_DCID);
        assert!(result.is_err());
        if let Err(QuicError::InvalidPacket(msg)) = result {
            assert!(msg.contains("Invalid first byte"), "Unexpected message: {}", msg);
        } else {
            panic!("Expected InvalidPacket error for wrong type, got {:?}", result);
        }
    }

    #[test]
    fn test_retry_from_bytes_empty_token() {
        let mut data = vec![0xFA]; 
        data.extend_from_slice(&0x00000001u32.to_be_bytes()); 
        data.push(0); 
        data.push(0); 
        data.extend_from_slice(&MOCK_RETRY_TAG);
        let result = RetryPacket::from_bytes(&data, MOCK_ORIGINAL_CLIENT_DCID);
        assert!(result.is_err());
        if let Err(QuicError::InvalidPacket(msg)) = result {
            assert_eq!(msg, "Retry Packet: Data too short for minimal structure");
        } else {
            panic!("Expected InvalidPacket error for empty token, got {:?}", result);
        }
    }
    #[test]
    fn test_retry_to_bytes_empty_token_error() {
         let packet = RetryPacket {
            version: 0x00000001,
            destination_connection_id: vec![0x11],
            source_connection_id: vec![0x33],
            retry_token: vec![], 
            retry_integrity_tag: MOCK_RETRY_TAG,
        };
        assert!(packet.to_bytes(MOCK_ORIGINAL_CLIENT_DCID).is_err());
    }

    #[test]
    fn test_retry_from_bytes_too_short_for_tag() {
        let data = vec![0xFA, 0,0,0,1, 0,0, 1, 0xAA, 0xBB]; 
        let result = RetryPacket::from_bytes(&data, MOCK_ORIGINAL_CLIENT_DCID);
        assert!(result.is_err());
         if let Err(QuicError::InvalidPacket(msg)) = result { 
            assert!(msg.contains("Retry Packet: Data too short for minimal structure"), "Unexpected InvalidPacket message: {}", msg);
        } else {
            panic!("Expected InvalidPacket for data too short for tag, got {:?}", result);
        }
    }
}

#[cfg(test)]
mod short_header_tests {
use crate::net::quic::frame::{Frame, CryptoFrame};
    use super::*;

    fn mock_short_packet(
        spin: bool, 
        key_phase: bool, 
        dcid: Vec<u8>, 
        pn: u32
    ) -> ShortHeaderPacket {
        ShortHeaderPacket {
            spin_bit: spin,
            key_phase_bit: key_phase,
            destination_connection_id: dcid,
            packet_number: pn,
        }
    }

    #[test]
    fn test_from_bytes_valid_pn_len1() {
        let dcid = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut data = vec![0x40]; 
        data.extend_from_slice(&dcid);
        data.extend_from_slice(&[0x12]); 
        data.extend_from_slice(&[0xff, 0xee]); 

        let (packet, payload) = ShortHeaderPacket::from_bytes(&data, dcid.len()).unwrap();

        assert_eq!(packet.spin_bit, false);
        assert_eq!(packet.key_phase_bit, false);
        assert_eq!(packet.destination_connection_id, dcid);
        assert_eq!(packet.packet_number, 0x12);
        assert_eq!(payload, &[0xff, 0xee]);
    }

    #[test]
    fn test_from_bytes_valid_pn_len4_spin_key_phase() {
        let dcid = vec![0xa1, 0xa2, 0xa3, 0xa4];
        let mut data = vec![0x73]; 
        data.extend_from_slice(&dcid);
        data.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); 
        data.extend_from_slice(&[0xaa, 0xbb, 0xcc]); 

        let (packet, payload) = ShortHeaderPacket::from_bytes(&data, dcid.len()).unwrap();

        assert_eq!(packet.spin_bit, true);
        assert_eq!(packet.key_phase_bit, true);
        assert_eq!(packet.destination_connection_id, dcid);
        assert_eq!(packet.packet_number, 0x12345678);
        assert_eq!(payload, &[0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn test_from_bytes_invalid_header_form() {
        let data = vec![0xC0, 1, 2, 3, 4, 5]; 
        let result = ShortHeaderPacket::from_bytes(&data, 4);
        assert!(result.is_err());
        if let Err(QuicError::InvalidPacket(msg)) = result {
            assert!(msg.contains("Invalid Header Form"), "Unexpected message: {}", msg);
        } else {
            panic!("Expected InvalidPacket for wrong header form, got {:?}", result);
        }
    }

    #[test]
    fn test_from_bytes_invalid_fixed_bit() {
        let data = vec![0x00, 1, 2, 3, 4, 5]; 
        let result = ShortHeaderPacket::from_bytes(&data, 4);
        assert!(result.is_err());
        if let Err(QuicError::InvalidPacket(msg)) = result {
            assert!(msg.contains("Invalid Header Form"), "Unexpected message: {}", msg); 
        } else {
            panic!("Expected InvalidPacket for wrong fixed bit, got {:?}", result);
        }
    }

    #[test]
    fn test_from_bytes_invalid_reserved_bits() {
        let data = vec![0x4C, 1, 2, 3, 4, 0x12]; 
        let result = ShortHeaderPacket::from_bytes(&data, 4);
        assert!(result.is_err());
        if let Err(QuicError::InvalidPacket(msg)) = result {
            assert!(msg.contains("Reserved bits must be zero"), "Unexpected message: {}", msg);
        } else {
            panic!("Expected InvalidPacket for non-zero reserved bits, got {:?}", result);
        }
    }

    #[test]
    fn test_from_bytes_too_short_for_dcid() {
        let data = vec![0x40]; 
        let result = ShortHeaderPacket::from_bytes(&data, 8); 
        assert!(result.is_err());
        if let Err(QuicError::ParseError(msg)) = result {
            assert!(msg.contains("Data too short for DCID"), "Unexpected message: {}", msg);
        } else {
            panic!("Expected ParseError for insufficient DCID data, got {:?}", result);
        }
    }

    #[test]
    fn test_from_bytes_too_short_for_pn() {
        let dcid = vec![1,2,3,4];
        let mut data = vec![0x43]; 
        data.extend_from_slice(&dcid);
        data.extend_from_slice(&[0x12, 0x34]); 
        let result = ShortHeaderPacket::from_bytes(&data, dcid.len());
        assert!(result.is_err());
        if let Err(QuicError::ParseError(msg)) = result {
            assert!(msg.contains("Data too short for Packet Number"), "Unexpected message: {}", msg);
        } else {
            panic!("Expected ParseError for insufficient PN data, got {:?}", result);
        }
    }

    #[test]
    fn test_to_bytes_and_from_bytes_round_trip() {
        let dcid = vec![0xde, 0xad, 0xbe, 0xef];
        let packet_orig = mock_short_packet(true, false, dcid.clone(), 0x1234); 
        let frame_payload = Frame::Crypto(CryptoFrame { offset: 0, data: vec![0xca, 0xfe, 0xba, 0xbe] });
        let frames_to_send = vec![frame_payload.clone()];
        let bytes = packet_orig.to_bytes(&frames_to_send).unwrap(); 

        assert_eq!(bytes[0], 0x61, "First byte mismatch"); 
        let (packet_parsed, payload_parsed) = ShortHeaderPacket::from_bytes(&bytes, dcid.len()).unwrap();
        assert_eq!(packet_parsed, packet_orig, "Parsed packet mismatch");
        let mut original_frames_bytes = Vec::new();
        for frame in &frames_to_send {
            original_frames_bytes.extend_from_slice(&frame.to_bytes());
        }
        assert_eq!(payload_parsed, original_frames_bytes.as_slice(), "Parsed payload mismatch");
        
    }

    #[test]
    fn test_to_bytes_pn_len_variants() {
        let dcid = vec![1,2,3,4];
        let payload_frames = vec![Frame::Ping]; let payload_bytes_len = payload_frames[0].to_bytes().len();

        let p1 = mock_short_packet(false, false, dcid.clone(), 0xab); 
        let b1 = p1.to_bytes(&payload_frames).unwrap();
        assert_eq!(b1[0], 0x40); 
        assert_eq!(b1.len(), 1 + dcid.len() + 1 + payload_bytes_len);
        let (p1_p, _) = ShortHeaderPacket::from_bytes(&b1, dcid.len()).unwrap();
        assert_eq!(p1_p.packet_number, 0xab);

        let p2 = mock_short_packet(true, false, dcid.clone(), 0xabcd); 
        let b2 = p2.to_bytes(&payload_frames).unwrap();
        assert_eq!(b2[0], 0x61); 
        assert_eq!(b2.len(), 1 + dcid.len() + 2 + payload_bytes_len);
        let (p2_p, _) = ShortHeaderPacket::from_bytes(&b2, dcid.len()).unwrap();
        assert_eq!(p2_p.packet_number, 0xabcd);

        let p3 = mock_short_packet(false, true, dcid.clone(), 0xabcdef); 
        let b3 = p3.to_bytes(&payload_frames).unwrap();
        assert_eq!(b3[0], 0x52); 
        assert_eq!(b3.len(), 1 + dcid.len() + 3 + payload_bytes_len);
        let (p3_p, _) = ShortHeaderPacket::from_bytes(&b3, dcid.len()).unwrap();
        assert_eq!(p3_p.packet_number, 0xabcdef);

        let p4 = mock_short_packet(true, true, dcid.clone(), 0xabcdef12); 
        let b4 = p4.to_bytes(&payload_frames).unwrap();
        assert_eq!(b4[0], 0x73); 
        assert_eq!(b4.len(), 1 + dcid.len() + 4 + payload_bytes_len);
        let (p4_p, _) = ShortHeaderPacket::from_bytes(&b4, dcid.len()).unwrap();
        assert_eq!(p4_p.packet_number, 0xabcdef12);
    }
}