//! H264 bitstream utilities shared across platform-specific encoder backends.
//!
//! VideoToolbox (macOS) and VAAPI (Linux) H264 hardware encoders output
//! AVCC format (length-prefixed NALUs). WebRTC expects Annex B format
//! (start-code-prefixed NALUs). This module handles the conversion.

const ANNEXB_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Parse AVCC extradata (from FFmpeg's AVCodecContext.extradata) to extract
/// SPS and PPS NALUs in Annex B format.
///
/// Returns `(sps_pps_annexb, nalu_length_size)`. The SPS/PPS data should be
/// prepended to keyframes so decoders can initialize without waiting for
/// in-band parameter sets.
pub fn parse_avcc_extradata(extradata: &[u8]) -> (Vec<u8>, usize) {
    if extradata.len() < 7 {
        return (Vec::new(), 4);
    }

    let nalu_length_size = ((extradata[4] & 0x03) + 1) as usize;
    let mut annexb = Vec::new();
    let mut offset = 5;

    // SPS entries
    let num_sps = (extradata[offset] & 0x1f) as usize;
    offset += 1;
    for _ in 0..num_sps {
        if offset + 2 > extradata.len() {
            break;
        }
        let len = u16::from_be_bytes([extradata[offset], extradata[offset + 1]]) as usize;
        offset += 2;
        if offset + len > extradata.len() {
            break;
        }
        annexb.extend_from_slice(&ANNEXB_START_CODE);
        annexb.extend_from_slice(&extradata[offset..offset + len]);
        offset += len;
    }

    // PPS entries
    if offset >= extradata.len() {
        return (annexb, nalu_length_size);
    }
    let num_pps = extradata[offset] as usize;
    offset += 1;
    for _ in 0..num_pps {
        if offset + 2 > extradata.len() {
            break;
        }
        let len = u16::from_be_bytes([extradata[offset], extradata[offset + 1]]) as usize;
        offset += 2;
        if offset + len > extradata.len() {
            break;
        }
        annexb.extend_from_slice(&ANNEXB_START_CODE);
        annexb.extend_from_slice(&extradata[offset..offset + len]);
        offset += len;
    }

    (annexb, nalu_length_size)
}

/// Convert H264 packet data from AVCC format (N-byte big-endian length prefixes)
/// to Annex B format (00 00 00 01 start codes).
///
/// If `keyframe_header` is provided, it is prepended to the output (used to
/// inject SPS/PPS before IDR frames).
pub fn avcc_to_annexb(
    avcc_data: &[u8],
    nalu_length_size: usize,
    keyframe_header: Option<&[u8]>,
) -> Vec<u8> {
    let mut annexb = Vec::with_capacity(avcc_data.len() + keyframe_header.map_or(0, |h| h.len()));

    if let Some(header) = keyframe_header {
        annexb.extend_from_slice(header);
    }

    let mut offset = 0;
    while offset + nalu_length_size <= avcc_data.len() {
        let nalu_len = match nalu_length_size {
            4 => u32::from_be_bytes([
                avcc_data[offset],
                avcc_data[offset + 1],
                avcc_data[offset + 2],
                avcc_data[offset + 3],
            ]) as usize,
            2 => u16::from_be_bytes([avcc_data[offset], avcc_data[offset + 1]]) as usize,
            1 => avcc_data[offset] as usize,
            _ => break,
        };
        offset += nalu_length_size;

        if nalu_len == 0 || offset + nalu_len > avcc_data.len() {
            break;
        }

        annexb.extend_from_slice(&ANNEXB_START_CODE);
        annexb.extend_from_slice(&avcc_data[offset..offset + nalu_len]);
        offset += nalu_len;
    }

    annexb
}

/// Check if data starts with an Annex B start code.
pub fn is_annexb(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avcc_to_annexb_single_nalu() {
        // 4-byte length prefix: 0x00000005 = 5 bytes
        let avcc = [0, 0, 0, 5, 0x65, 1, 2, 3, 4];
        let result = avcc_to_annexb(&avcc, 4, None);
        assert_eq!(result, [0, 0, 0, 1, 0x65, 1, 2, 3, 4]);
    }

    #[test]
    fn test_avcc_to_annexb_two_nalus() {
        let avcc = [
            0, 0, 0, 2, 0x67, 0xAA, // SPS (2 bytes)
            0, 0, 0, 3, 0x65, 1, 2, // IDR (3 bytes)
        ];
        let result = avcc_to_annexb(&avcc, 4, None);
        assert_eq!(result, [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x65, 1, 2]);
    }

    #[test]
    fn test_avcc_to_annexb_with_keyframe_header() {
        let sps_pps = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        let avcc = [0, 0, 0, 2, 0x65, 0xFF];
        let result = avcc_to_annexb(&avcc, 4, Some(&sps_pps));
        assert_eq!(
            result,
            [
                0, 0, 0, 1, 0x67, 0x42, // SPS from header
                0, 0, 0, 1, 0x68, 0xCE, // PPS from header
                0, 0, 0, 1, 0x65, 0xFF, // IDR from packet
            ]
        );
    }

    #[test]
    fn test_parse_avcc_extradata() {
        // Minimal AVCC: version=1, profile=66, compat=0, level=30, length_size=4
        // 1 SPS (2 bytes: 0x67 0x42), 1 PPS (2 bytes: 0x68 0xCE)
        let extradata = [
            1, 66, 0, 30, 0xFF, // header (length_size = (0xFF & 3) + 1 = 4)
            0xE1, // 1 SPS (0xE1 & 0x1F = 1)
            0, 2, 0x67, 0x42, // SPS length=2, data
            1,    // 1 PPS
            0, 2, 0x68, 0xCE, // PPS length=2, data
        ];
        let (annexb, nalu_len_size) = parse_avcc_extradata(&extradata);
        assert_eq!(nalu_len_size, 4);
        assert_eq!(annexb, [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE]);
    }

    #[test]
    fn test_parse_avcc_extradata_empty() {
        let (annexb, nalu_len_size) = parse_avcc_extradata(&[]);
        assert!(annexb.is_empty());
        assert_eq!(nalu_len_size, 4);
    }

    #[test]
    fn test_avcc_to_annexb_2byte_length() {
        // 2-byte length prefix: 0x0003 = 3 bytes
        let avcc = [0, 3, 0x65, 1, 2];
        let result = avcc_to_annexb(&avcc, 2, None);
        assert_eq!(result, [0, 0, 0, 1, 0x65, 1, 2]);
    }

    #[test]
    fn test_avcc_to_annexb_truncated() {
        // Length says 10 bytes but only 2 available
        let avcc = [0, 0, 0, 10, 0x65, 0xFF];
        let result = avcc_to_annexb(&avcc, 4, None);
        assert!(result.is_empty()); // Truncated, no output
    }
}
