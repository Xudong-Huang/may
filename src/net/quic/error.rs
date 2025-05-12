// QUIC specific error types and handling

#[derive(Debug)]
pub enum QuicError {
    ParseError(String),
    InvalidPacket(String),
    UnknownFrameType(u8),
    FrameEncodingError(String),
    // Add more specific errors later
}

impl std::fmt::Display for QuicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            &QuicError::ParseError(ref s) => write!(f, "QUIC Parse Error: {}", s),
            &QuicError::InvalidPacket(ref s) => write!(f, "Invalid QUIC Packet: {}", s),
            &QuicError::UnknownFrameType(t) => write!(f, "Unknown QUIC Frame Type: {:#02x}", t),
            &QuicError::FrameEncodingError(ref s) => write!(f, "QUIC Frame Encoding Error: {}", s),
        }
    }
}

impl std::error::Error for QuicError {}