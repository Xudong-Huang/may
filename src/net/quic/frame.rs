// QUIC frame definitions and parsing logic
// RFC 9000 Section 12.4 & 19

use super::error::QuicError;
use super::packet::{decode_varint, encode_varint};

/// Represents the data and properties of a STREAM frame.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct StreamFrame {
    pub stream_id: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub fin: bool,
}

/// Represents an ACK Range block in an ACK frame.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AckRange {
    pub gap: u64,
    pub ack_range_length: u64,
}

/// Represents ECN counts in an ACK frame.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct EcnCounts {
    pub ect0_count: u64,
    pub ect1_count: u64,
    pub ecn_ce_count: u64,
}

/// Represents an ACK frame.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AckFrame {
    pub largest_acknowledged: u64,
    pub ack_delay: u64,
    pub first_ack_range: u64,
    pub ack_ranges: Vec<AckRange>,
    pub ecn_counts: Option<EcnCounts>,
}

/// Represents a CRYPTO frame.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct CryptoFrame {
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Represents a CONNECTION_CLOSE frame.
/// RFC 9000 Section 19.19
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ConnectionCloseFrame {
    pub error_code: u64,
    /// This field is present only if the frame type is 0x1c (QUIC layer error).
    /// For frame type 0x1d (application error), this is None.
    pub frame_type_field: Option<u64>, // Renamed from frame_type to avoid confusion
    pub reason_phrase: Vec<u8>,
}

// RFC 9000 Section 19.4
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ResetStreamFrame {
    pub stream_id: u64,
    pub application_protocol_error_code: u64,
    pub final_size: u64,
}
/// Represents different types of QUIC frames.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Frame {
    Padding,
    Ping,
    Ack(AckFrame),
    Crypto(CryptoFrame),
    Stream(StreamFrame),
    ConnectionClose(ConnectionCloseFrame),
    
    // TODO: Add other frame types:
    ResetStream(ResetStreamFrame),
    // StopSending(StopSendingFrame),
    // NewToken(NewTokenFrame),
    // MaxData(MaxDataFrame),
    // MaxStreamData(MaxStreamDataFrame),
    // MaxStreams(MaxStreamsFrame),
    // DataBlocked(DataBlockedFrame),
    // StreamDataBlocked(StreamDataBlockedFrame),
    // StreamsBlocked(StreamsBlockedFrame),
    // NewConnectionId(NewConnectionIdFrame),
    // RetireConnectionId(RetireConnectionIdFrame),
    // PathChallenge(PathChallengeFrame),
    // PathResponse(PathResponseFrame),
    // HandshakeDone(HandshakeDoneFrame),
}


impl Frame {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Frame::Padding => vec![0x00],
            Frame::Ping => vec![0x01],
            Frame::Stream(sf) => {
                let mut frame_type_byte: u8 = 0x08;
                let mut header_parts: Vec<Vec<u8>> = Vec::new();
                if sf.fin { frame_type_byte |= 0x01; }
                if sf.offset > 0 { frame_type_byte |= 0x04; }
// In this implementation, the LEN bit (0x02) is always set,
                // meaning the Length field is always present.
                // RFC 9000 Section 19.8 allows for the LEN bit to be 0 if the Stream
                // frame extends to the end of the packet, omitting the Length field.
                // Future optimizations could involve a mechanism (e.g., an additional
                // parameter to to_bytes or context from the packet packer) to enable
                // this for the last frame in a packet.
                frame_type_byte |= 0x02;
                header_parts.push(encode_varint(sf.stream_id));
                if sf.offset > 0 {
                    header_parts.push(encode_varint(sf.offset));
                }
                header_parts.push(encode_varint(sf.data.len() as u64));
                let mut bytes = Vec::with_capacity(1 + header_parts.iter().map(|p| p.len()).sum::<usize>() + sf.data.len());
                bytes.push(frame_type_byte);
                for part in header_parts {
                    bytes.extend_from_slice(&part);
                }
                bytes.extend_from_slice(&sf.data);
                bytes
            }
            Frame::Ack(af) => {
                let frame_type_byte: u8 = if af.ecn_counts.is_some() { 0x03 } else { 0x02 };
                let mut bytes = Vec::new();
                bytes.push(frame_type_byte);
                bytes.extend_from_slice(&encode_varint(af.largest_acknowledged));
                bytes.extend_from_slice(&encode_varint(af.ack_delay));
                bytes.extend_from_slice(&encode_varint(af.ack_ranges.len() as u64));
                bytes.extend_from_slice(&encode_varint(af.first_ack_range));
                for ack_range in &af.ack_ranges {
                    bytes.extend_from_slice(&encode_varint(ack_range.gap));
                    bytes.extend_from_slice(&encode_varint(ack_range.ack_range_length));
                }
                if let Some(ecn) = &af.ecn_counts {
                    bytes.extend_from_slice(&encode_varint(ecn.ect0_count));
                    bytes.extend_from_slice(&encode_varint(ecn.ect1_count));
                    bytes.extend_from_slice(&encode_varint(ecn.ecn_ce_count));
                }
                bytes
            }
            Frame::Crypto(cf) => {
                let mut bytes = Vec::new();
                bytes.push(0x06);
                bytes.extend_from_slice(&encode_varint(cf.offset));
                bytes.extend_from_slice(&encode_varint(cf.data.len() as u64));
                bytes.extend_from_slice(&cf.data);
                bytes
            }
            Frame::ConnectionClose(ccf) => {
                let frame_type_byte: u8 = if ccf.frame_type_field.is_some() { 0x1c } else { 0x1d };
                let mut bytes = Vec::new();
                bytes.push(frame_type_byte);
                bytes.extend_from_slice(&encode_varint(ccf.error_code));
                if let Some(ft) = ccf.frame_type_field {
                    bytes.extend_from_slice(&encode_varint(ft));
                }
                bytes.extend_from_slice(&encode_varint(ccf.reason_phrase.len() as u64));
                bytes.extend_from_slice(&ccf.reason_phrase);
                bytes
            }
Frame::ResetStream(rsf) => {
                let mut bytes = Vec::new();
                bytes.push(0x04); // RESET_STREAM Frame Type
                bytes.extend_from_slice(&encode_varint(rsf.stream_id));
                bytes.extend_from_slice(&encode_varint(rsf.application_protocol_error_code));
                bytes.extend_from_slice(&encode_varint(rsf.final_size));
                bytes
            }
        }
    }

    pub fn from_bytes(data: &[u8]) -> Result<(Self, usize), QuicError> {
        if data.is_empty() {
            return Err(QuicError::ParseError("Cannot parse frame from empty data".to_string()));
        }

        let frame_type_byte = data[0];
        let mut current_payload_data = &data[1..]; 
        let mut consumed_total = 1;

        match frame_type_byte {
            0x00 => Ok((Frame::Padding, 1)),
            0x01 => Ok((Frame::Ping, 1)),
            0x02 | 0x03 => { 
                let (largest_acknowledged, consumed) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode Largest Acknowledged: {}", e)))?;
                consumed_total += consumed;
                let (ack_delay, consumed) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode ACK Delay: {}", e)))?;
                consumed_total += consumed;
                let (ack_range_count, consumed) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode ACK Range Count: {}", e)))?;
                consumed_total += consumed;
                let (first_ack_range, consumed) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode First ACK Range: {}", e)))?;
                consumed_total += consumed;
                let mut ack_ranges_vec = Vec::with_capacity(ack_range_count as usize);
                for _ in 0..ack_range_count {
                    let (gap, consumed_gap) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode Gap: {}", e)))?;
                    consumed_total += consumed_gap;
                    let (ack_range_length, consumed_arl) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("ACK: Failed to decode ACK Range Length: {}", e)))?;
                    consumed_total += consumed_arl;
                    ack_ranges_vec.push(AckRange { gap, ack_range_length });
                }
                let ecn_counts_opt = if frame_type_byte == 0x03 {
                    let (ect0_count, consumed) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("ACK ECN: Failed to decode ECT0 Count: {}", e)))?;
                    consumed_total += consumed;
                    let (ect1_count, consumed) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("ACK ECN: Failed to decode ECT1 Count: {}", e)))?;
                    consumed_total += consumed;
                    let (ecn_ce_count, consumed) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("ACK ECN: Failed to decode ECN-CE Count: {}", e)))?;
                    consumed_total += consumed;
                    Some(EcnCounts { ect0_count, ect1_count, ecn_ce_count })
                } else { None };
                Ok((Frame::Ack(AckFrame { largest_acknowledged, ack_delay, first_ack_range, ack_ranges: ack_ranges_vec, ecn_counts: ecn_counts_opt }), consumed_total )),
0x04 => { // RESET_STREAM Frame
                let (stream_id, consumed_sid) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("RESET_STREAM: Failed to decode Stream ID: {}", e)))?;
                consumed_total += consumed_sid;

                let (app_error_code, consumed_aec) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("RESET_STREAM: Failed to decode Application Protocol Error Code: {}", e)))?;
                consumed_total += consumed_aec;

                let (final_size, consumed_fs) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("RESET_STREAM: Failed to decode Final Size: {}", e)))?;
                consumed_total += consumed_fs;

                Ok((
                    Frame::ResetStream(ResetStreamFrame {
                        stream_id,
                        application_protocol_error_code: app_error_code,
                        final_size,
                    }),
                    consumed_total,
                ))
            },
            
            0x06 => { 
                let (offset_val, consumed_off) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("CRYPTO: Failed to decode Offset: {}", e)))?;
                consumed_total += consumed_off;
                let (length_val, consumed_len_field) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("CRYPTO: Failed to decode Length: {}", e)))?;
                consumed_total += consumed_len_field;
                let data_len = length_val as usize;
                if current_payload_data.len() < data_len {
                    return Err(QuicError::ParseError(format!("CRYPTO: Not enough data for Crypto Data. Expected {}, got {}", data_len, current_payload_data.len())));
                }
                let crypto_data_vec = current_payload_data[..data_len].to_vec();
                consumed_total += data_len;
                Ok((Frame::Crypto(CryptoFrame { offset: offset_val, data: crypto_data_vec, }), consumed_total ))
            }
            0x08..=0x0f => { 
                let fin_bit = (frame_type_byte & 0x01) != 0;
                let len_bit = (frame_type_byte & 0x02) != 0;
                let off_bit = (frame_type_byte & 0x04) != 0;
                let (stream_id_val, consumed_sid) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("STREAM: Failed to decode Stream ID: {}", e)))?;
                consumed_total += consumed_sid;
                let offset_val = if off_bit {
                    let (val, consumed_off) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("STREAM: Failed to decode Offset: {}", e)))?;
                    consumed_total += consumed_off;
                    val
                } else { 0 };
                let data_vec;
                if len_bit {
                    let (length_val, consumed_len_field) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("STREAM: Failed to decode Length: {}", e)))?;
                    consumed_total += consumed_len_field;
// Check if offset + length exceeds 2^62-1
// RFC 9000 Section 19.8: "The largest offset delivered on a stream --
// the sum of the offset and data length -- cannot exceed 2^62-1"
const MAX_STREAM_OFFSET_SUM: u64 = (1u64 << 62) - 1;
if offset_val > MAX_STREAM_OFFSET_SUM || length_val > MAX_STREAM_OFFSET_SUM - offset_val {
    return Err(QuicError::FrameEncodingError(
        format!("STREAM frame offset + length ({_offset} + {_length}) exceeds 2^62-1", _offset = offset_val, _length = length_val)
    ));
}
                    let data_len = length_val as usize;
                    if current_payload_data.len() < data_len {
                        return Err(QuicError::ParseError(format!("STREAM: Not enough data for Stream Data. Expected {}, got {}", data_len, current_payload_data.len())));
                    }
                    data_vec = current_payload_data[..data_len].to_vec();
                    consumed_total += data_len;
                } else {
                    data_vec = current_payload_data.to_vec();
                    consumed_total += current_payload_data.len();
                }
                Ok((Frame::Stream(StreamFrame { stream_id: stream_id_val, offset: offset_val, data: data_vec, fin: fin_bit, }), consumed_total))
            }
            0x1c | 0x1d => { // CONNECTION_CLOSE frames
                let (error_code_val, consumed_ec) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("CONNECTION_CLOSE: Failed to decode Error Code: {}", e)))?;
                consumed_total += consumed_ec;

                let frame_type_field_opt = if frame_type_byte == 0x1c {
                    let (ft_val, consumed_ft) = decode_varint(&mut current_payload_data)
                        .map_err(|e| QuicError::ParseError(format!("CONNECTION_CLOSE (0x1c): Failed to decode Frame Type: {}", e)))?;
                    consumed_total += consumed_ft;
                    Some(ft_val)
                } else {
                    None
                };

                let (reason_len_val, consumed_rl) = decode_varint(&mut current_payload_data)
                    .map_err(|e| QuicError::ParseError(format!("CONNECTION_CLOSE: Failed to decode Reason Phrase Length: {}", e)))?;
                consumed_total += consumed_rl;

                let reason_len = reason_len_val as usize;
                if current_payload_data.len() < reason_len {
                    return Err(QuicError::ParseError(format!(
                        "CONNECTION_CLOSE: Not enough data for Reason Phrase. Expected {}, got {}",
                        reason_len,
                        current_payload_data.len()
                    )));
                }
                let reason_phrase_vec = current_payload_data[..reason_len].to_vec();
                consumed_total += reason_len;

                Ok((
                    Frame::ConnectionClose(ConnectionCloseFrame {
                        error_code: error_code_val,
                        frame_type_field: frame_type_field_opt,
                        reason_phrase: reason_phrase_vec,
                    }),
                    consumed_total,
                ))
            }
            _ => Err(QuicError::UnknownFrameType(frame_type_byte)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_padding_frame_to_bytes() {
        let frame = Frame::Padding;
        assert_eq!(frame.to_bytes(), vec![0x00]);
    }

    #[test]
    fn test_ping_frame_to_bytes() {
        let frame = Frame::Ping;
        assert_eq!(frame.to_bytes(), vec![0x01]);
    }

    #[test]
    fn test_frame_from_bytes_padding() {
        let data = [0x00, 0x01, 0x02]; 
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(frame, Frame::Padding);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn test_frame_from_bytes_ping() {
        let data = [0x01, 0xaa, 0xbb]; 
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(frame, Frame::Ping);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn test_frame_from_bytes_empty_data() {
        let data = [];
        let result = Frame::from_bytes(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_frame_from_bytes_unknown_type() {
        let data = [0xff, 0x01, 0x02]; 
        let result = Frame::from_bytes(&data);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_stream_frame_to_bytes_all_flags_no_offset_empty_data() {
        let frame = Frame::Stream(StreamFrame { stream_id: 1, offset: 0, data: vec![], fin: true });
        assert_eq!(frame.to_bytes(), vec![0x0B, 0x01, 0x00]);
    }
    
    #[test]
    fn test_stream_frame_from_bytes_all_flags_no_offset_empty_data() {
        let data = [0x0B, 0x01, 0x00, 0xAA];
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(frame, Frame::Stream(StreamFrame { stream_id: 1, offset: 0, data: vec![], fin: true }));
    }

    #[test]
    fn test_ack_frame_to_bytes_no_ecn_no_additional_ranges() {
        let frame = Frame::Ack(AckFrame { largest_acknowledged: 100, ack_delay: 25, first_ack_range: 5, ack_ranges: vec![], ecn_counts: None });
        assert_eq!(frame.to_bytes(), vec![0x02, 0x64, 0x19, 0x00, 0x05]);
    }

    #[test]
    fn test_ack_frame_from_bytes_no_ecn_no_additional_ranges() {
        let data = [0x02, 0x64, 0x19, 0x00, 0x05, 0xFF];
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(frame, Frame::Ack(AckFrame { largest_acknowledged: 100, ack_delay: 25, first_ack_range: 5, ack_ranges: vec![], ecn_counts: None }));
    }
    
    #[test]
    fn test_crypto_frame_to_bytes() {
        let frame = Frame::Crypto(CryptoFrame { offset: 1024, data: vec![0xCA, 0xFE, 0xBA, 0xBE] });
        assert_eq!(frame.to_bytes(), vec![0x06, 0x44, 0x00, 0x04, 0xCA, 0xFE, 0xBA, 0xBE]);
    }

    #[test]
    fn test_crypto_frame_from_bytes() {
        let data = [0x06, 0x44, 0x00, 0x04, 0xCA, 0xFE, 0xBA, 0xBE, 0xDE];
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(consumed, 8);
        assert_eq!(frame, Frame::Crypto(CryptoFrame { offset: 1024, data: vec![0xCA, 0xFE, 0xBA, 0xBE] }));
    }

    // --- CONNECTION_CLOSE Frame Tests ---
    #[test]
    fn test_connection_close_quic_layer_to_bytes() {
        let frame = Frame::ConnectionClose(ConnectionCloseFrame {
            error_code: 0x0A, // PROTOCOL_VIOLATION
            frame_type_field: Some(0x08), // STREAM frame
            reason_phrase: b"test reason".to_vec(),
        });
        let expected_bytes = vec![
            0x1c, 0x0A, 0x08, 0x0B, 
            b't', b'e', b's', b't', b' ', b'r', b'e', b'a', b's', b'o', b'n'
        ];
        assert_eq!(frame.to_bytes(), expected_bytes);
    }

    #[test]
    fn test_connection_close_quic_layer_from_bytes() {
        let data = [
            0x1c, 0x0A, 0x08, 0x0B, 
            b't', b'e', b's', b't', b' ', b'r', b'e', b'a', b's', b'o', b'n',
            0xEE, // Extra byte
        ];
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(consumed, 1 + 1 + 1 + 1 + 11);
        match frame {
            Frame::ConnectionClose(ccf) => {
                assert_eq!(ccf.error_code, 0x0A);
                assert_eq!(ccf.frame_type_field, Some(0x08));
                assert_eq!(ccf.reason_phrase, b"test reason".to_vec());
            }
            _ => panic!("Expected ConnectionCloseFrame"),
        }
    }

    #[test]
    fn test_connection_close_app_layer_to_bytes() {
        let error_val = 0x1234ABCDu64; 
        let error_code_bytes = encode_varint(error_val);
        let reason_phrase_val = b"app close here".to_vec();
        let reason_len_bytes = encode_varint(reason_phrase_val.len() as u64);

        let frame = Frame::ConnectionClose(ConnectionCloseFrame {
            error_code: error_val, 
            frame_type_field: None,
            reason_phrase: reason_phrase_val.clone(),
        });
        
        let mut expected_vec = vec![0x1d];
        expected_vec.extend_from_slice(&error_code_bytes);
        expected_vec.extend_from_slice(&reason_len_bytes);
        expected_vec.extend_from_slice(&reason_phrase_val);
        
        assert_eq!(frame.to_bytes(), expected_vec);
    }
    
    #[test]
    fn test_connection_close_app_layer_from_bytes() {
        let error_val = 0x7ABCu64; 
        let error_code_bytes = encode_varint(error_val);
        let reason_phrase = b"application error test";
        let reason_len_bytes = encode_varint(reason_phrase.len() as u64);
        
        let mut data_vec = vec![0x1d];
        data_vec.extend_from_slice(&error_code_bytes);
        data_vec.extend_from_slice(&reason_len_bytes);
        data_vec.extend_from_slice(reason_phrase);
        data_vec.push(0xFF); 
        
        let (frame, consumed) = Frame::from_bytes(&data_vec).unwrap();
        let expected_consumed = 1 + error_code_bytes.len() + reason_len_bytes.len() + reason_phrase.len();
        assert_eq!(consumed, expected_consumed);

        match frame {
            Frame::ConnectionClose(ccf) => {
                assert_eq!(ccf.error_code, error_val);
                assert!(ccf.frame_type_field.is_none());
                assert_eq!(ccf.reason_phrase, reason_phrase.to_vec());
            }
            _ => panic!("Expected ConnectionCloseFrame"),
        }
    }

    #[test]
    fn test_connection_close_quic_no_reason_to_bytes() {
         let frame = Frame::ConnectionClose(ConnectionCloseFrame {
            error_code: 0x01, 
            frame_type_field: Some(0x00), 
            reason_phrase: vec![],
        });
        let expected_bytes = vec![0x1c, 0x01, 0x00, 0x00];
        assert_eq!(frame.to_bytes(), expected_bytes);
    }

    #[test]
    fn test_connection_close_quic_no_reason_from_bytes() {
        let data = [0x1c, 0x01, 0x00, 0x00, 0xAA];
        let (frame, consumed) = Frame::from_bytes(&data).unwrap();
        assert_eq!(consumed, 4);
         match frame {
            Frame::ConnectionClose(ccf) => {
                assert_eq!(ccf.error_code, 0x01);
                assert_eq!(ccf.frame_type_field, Some(0x00));
                assert_eq!(ccf.reason_phrase, Vec::<u8>::new());
            }
            _ => panic!("Expected ConnectionCloseFrame"),
        }
    }
    
    #[test]
    fn test_connection_close_from_bytes_insufficient_data() {
        let data1 = [0x1c]; 
        assert!(Frame::from_bytes(&data1).is_err());

        let data2 = [0x1c, 0x01]; 
        assert!(Frame::from_bytes(&data2).is_err());

        let data3 = [0x1c, 0x01, 0x00]; 
        assert!(Frame::from_bytes(&data3).is_err());
        
        let data4 = [0x1d, 0x40, 0x01, 0x05, b't',b'e',b's',b't']; 
        assert!(Frame::from_bytes(&data4).is_err());
    }

    // --- Additional STREAM Frame Tests ---
    #[test]
    fn test_stream_frame_to_bytes_case1_offset_len_no_fin() {
        let stream_frame = StreamFrame {
            stream_id: 10,
            offset: 100,
            data: vec![1, 2, 3, 4, 5],
            fin: false,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE1_OLNF_A");
    }

    #[test]
    fn test_stream_frame_to_bytes_case2_no_offset_len_no_fin() {
        let stream_frame = StreamFrame {
            stream_id: 20,
            offset: 0,
            data: vec![10, 20],
            fin: false,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE2_NOLNF_A");
    }

    #[test]
    fn test_stream_frame_to_bytes_case3_offset_len_fin() {
        let stream_frame = StreamFrame {
            stream_id: 30,
            offset: 300,
            data: vec![1, 2, 3],
            fin: true,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE3_OLF_A");
    }

    #[test]
    fn test_stream_frame_to_bytes_case4_no_offset_len_fin() {
        let stream_frame = StreamFrame {
            stream_id: 40,
            offset: 0,
            data: vec![50],
            fin: true,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE4_NOLF_A");
    }
    
    #[test]
    fn test_stream_frame_to_bytes_case5_offset_len_fin_empty_data() {
        let stream_frame = StreamFrame {
            stream_id: 50,
            offset: 500,
            data: vec![],
            fin: true,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE5_OLFE_A");
    }

    #[test]
    fn test_stream_frame_to_bytes_case7_varint_boundaries() {
        let stream_frame = StreamFrame {
            stream_id: 63, 
            offset: 16383, 
            data: vec![0; 10], 
            fin: false,
        };
        assert_stream_frame_round_trip(stream_frame, "SF_CASE7_VARINT_A");
    }

    // --- from_bytes specific tests for LEN=0 ---
    #[test]
    fn test_stream_frame_from_bytes_case8_len0_offset_no_fin() {
        let test_data_bytes = vec![0x0C, 0x0C, 0x40, 0xC8, 10, 20, 30];
        let (parsed_frame, consumed) = Frame::from_bytes(&test_data_bytes).unwrap();
        assert_eq!(consumed, test_data_bytes.len());
        let expected_stream_frame = StreamFrame {
            stream_id: 12, offset: 200, data: vec![10, 20, 30], fin: false,
        };
        assert_eq!(parsed_frame, Frame::Stream(expected_stream_frame));
    }

    #[test]
    fn test_stream_frame_from_bytes_case9_len0_no_offset_fin() {
        let test_data_bytes = vec![0x09, 0x01, 99, 88];
        let (parsed_frame, consumed) = Frame::from_bytes(&test_data_bytes).unwrap();
        assert_eq!(consumed, test_data_bytes.len());
        let expected_stream_frame = StreamFrame {
            stream_id: 1, offset: 0, data: vec![99, 88], fin: true,
        };
        assert_eq!(parsed_frame, Frame::Stream(expected_stream_frame));
    }

    #[test]
    fn test_stream_frame_from_bytes_len0_no_offset_no_fin_empty_data() {
        let test_data_bytes = vec![0x08, 0x05];
        let (parsed_frame, consumed) = Frame::from_bytes(&test_data_bytes).unwrap();
        assert_eq!(consumed, test_data_bytes.len());
        let expected_stream_frame = StreamFrame {
            stream_id: 5, offset: 0, data: vec![], fin: false,
        };
        assert_eq!(parsed_frame, Frame::Stream(expected_stream_frame));
    }

    // --- Error/Boundary condition tests for from_bytes ---
    #[test]
    fn test_stream_frame_from_bytes_err_case10_insufficient_for_stream_id() {
        let data = [0x08]; 
        let result = Frame::from_bytes(&data);
        assert!(matches!(result, Err(QuicError::ParseError(_))));
    }

    #[test]
    fn test_stream_frame_from_bytes_err_case11_insufficient_for_offset() {
        let data = [0x0C, 0x01]; 
        let result = Frame::from_bytes(&data);
        assert!(matches!(result, Err(QuicError::ParseError(_))));
    }
    
    #[test]
    fn test_stream_frame_from_bytes_err_case12_insufficient_for_length() {
        let data = [0x0A, 0x01];
        let result = Frame::from_bytes(&data);
        assert!(matches!(result, Err(QuicError::ParseError(_))));
    }

    #[test]
    fn test_stream_frame_from_bytes_err_case13_insufficient_for_data() {
        let data = [0x0A, 0x01, 0x05, 1, 2, 3];
        let result = Frame::from_bytes(&data);
        assert!(matches!(result, Err(QuicError::ParseError(msg)) if msg.contains("Not enough data for Stream Data")));
    }
    
    // Helper for asserting StreamFrame round trip
    fn assert_stream_frame_round_trip(frame_struct: StreamFrame, test_case_name: &str) {
        let frame_enum = Frame::Stream(frame_struct.clone());
        let bytes = frame_enum.to_bytes();
        match Frame::from_bytes(&bytes) {
            Ok((parsed_frame, consumed)) => {
                assert_eq!(consumed, bytes.len(), "{}: Consumed length mismatch", test_case_name);
                assert_eq!(parsed_frame, Frame::Stream(frame_struct), "{}: Parsed frame mismatch", test_case_name);
            }
            Err(e) => {
                panic!("{}: from_bytes failed: {:?}", test_case_name, e);
            }
        }
    }

    #[test]
    fn test_stream_frame_round_trip_stream_id_varints() {
        // Test with stream_id at varint boundaries
        let stream_id_max_1_byte = (1u64 << 6) - 1;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_max_1_byte, offset: 0, data: vec![1], fin: false,
        }, "SF_SID_MAX_1B");

        let stream_id_min_2_byte = 1u64 << 6;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_min_2_byte, offset: 0, data: vec![2], fin: false,
        }, "SF_SID_MIN_2B");

        let stream_id_max_2_byte = (1u64 << 14) - 1;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_max_2_byte, offset: 0, data: vec![3], fin: false,
        }, "SF_SID_MAX_2B");

        let stream_id_min_4_byte = 1u64 << 14;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_min_4_byte, offset: 0, data: vec![4], fin: false,
        }, "SF_SID_MIN_4B");
        
        let stream_id_max_4_byte = (1u64 << 30) - 1;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_max_4_byte, offset: 0, data: vec![5], fin: false,
        }, "SF_SID_MAX_4B");

        let stream_id_min_8_byte = 1u64 << 30;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: stream_id_min_8_byte, offset: 0, data: vec![6], fin: false,
        }, "SF_SID_MIN_8B");

        // Per RFC 9000, Stream IDs are encoded as variable-length integers.
        // The largest possible Stream ID is 2^62-1.
        let max_stream_id = (1u64 << 62) - 1;
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: max_stream_id, offset: 0, data: vec![7], fin: false,
        }, "SF_SID_MAX_RFC");
    }

    #[test]
    fn test_stream_frame_len1_off1_fin1_various_lengths() {
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: 0x3F, 
            offset: 0x3FFF, 
            data: vec![0xDE, 0xAD, 0xBE, 0xEF], 
            fin: true,
        }, "LEN1_OFF1_FIN1_S1_O2_L1_D4_B"); 

        assert_stream_frame_round_trip(StreamFrame {
            stream_id: 0x7ABC, 
            offset: 0x3FFFFFFF, 
            data: vec![0xAA; 100], 
            fin: true,
        }, "LEN1_OFF1_FIN1_S2_O4_L2_D100_B");
    }

    #[test]
    fn test_stream_frame_len1_off0_fin0_various_lengths() {
         assert_stream_frame_round_trip(StreamFrame {
            stream_id: 0x3FFFFFFF,
            offset: 0,
            data: vec![],
            fin: false,
        }, "LEN1_OFF0_FIN0_S4_O0_L4_D0_B");

        let data_vec_large = vec![0x77; 17000]; 
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: 0x3FFFFFFFFFFFFFFF,
            offset: 0,
            data: data_vec_large.clone(),
            fin: false,
        }, "LEN1_OFF0_FIN0_S8_O0_L8_DLarge_B");
    }

    #[test]
    fn test_stream_frame_len1_off1_fin0_data_zero() {
         assert_stream_frame_round_trip(StreamFrame {
            stream_id: 100,
            offset: 200,
            data: vec![],
            fin: false,
        }, "LEN1_OFF1_FIN0_D0_B");
    }

    #[test]
    fn test_stream_frame_len1_off0_fin1_data_zero() {
        assert_stream_frame_round_trip(StreamFrame {
            stream_id: 1, 
            offset: 0, 
            data: vec![], 
            fin: true 
        }, "LEN1_OFF0_FIN1_D0_recheck_B");
    }
}
