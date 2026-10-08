//! DDC/CI packet codec. Byte layouts follow VESA DDC/CI 1.1; the expected
//! bytes in the tests come from the U3223QE spike (#2) and hand-worked
//! checksums.

/// 7-bit I2C address of the DDC/CI channel.
pub const DDC_CHIP: u8 = 0x37;
/// Host source address; also the register offset for reads and writes.
pub const HOST: u8 = 0x51;
/// The display's write address, which is also the first byte of its replies.
const DISPLAY: u8 = 0x6E;
/// Checksum seed for replies (the host's virtual read address).
const REPLY_SEED: u8 = 0x50;

const GET_VCP: u8 = 0x01;
const GET_VCP_REPLY: u8 = 0x02;
const SET_VCP: u8 = 0x03;
const CAPS: u8 = 0xF3;
const CAPS_REPLY: u8 = 0xE3;

/// Read sizes that hold a full reply.
pub const VCP_REPLY_LEN: usize = 12;
pub const CAPS_REPLY_LEN: usize = 38;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpValue {
    pub current: u16,
    pub max: u16,
}

impl VcpValue {
    /// Low byte of the current value. For VCP 0x60 this is the input code;
    /// the Dell U3223QE puts unrelated data in the high byte.
    pub fn low_byte(self) -> u8 {
        (self.current & 0xFF) as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    /// First byte is not the display address: the read is misaligned.
    BadSource(u8),
    BadLength,
    BadChecksum,
    /// The display sent a null message; it is busy, try again.
    Null,
    WrongOpcode(u8),
    WrongFeature(u8),
    WrongOffset(u16),
    /// The display does not support the feature.
    Unsupported,
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSource(b) => write!(f, "reply starts with 0x{b:02X}, not 0x6E"),
            Self::BadLength => write!(f, "reply length is invalid"),
            Self::BadChecksum => write!(f, "reply checksum is wrong"),
            Self::Null => write!(f, "display sent a null reply"),
            Self::WrongOpcode(o) => write!(f, "unexpected reply opcode 0x{o:02X}"),
            Self::WrongFeature(v) => write!(f, "reply is for VCP 0x{v:02X}"),
            Self::WrongOffset(o) => write!(f, "reply is for capabilities offset {o}"),
            Self::Unsupported => write!(f, "display does not support this feature"),
        }
    }
}

impl std::error::Error for ReplyError {}

pub fn get_vcp_request(vcp: u8) -> Vec<u8> {
    request(&[GET_VCP, vcp])
}

pub fn set_vcp_request(vcp: u8, value: u16) -> Vec<u8> {
    let [hi, lo] = value.to_be_bytes();
    request(&[SET_VCP, vcp, hi, lo])
}

pub fn capabilities_request(offset: u16) -> Vec<u8> {
    let [hi, lo] = offset.to_be_bytes();
    request(&[CAPS, hi, lo])
}

pub fn decode_vcp_reply(reply: &[u8], vcp: u8) -> Result<VcpValue, ReplyError> {
    let body = payload(reply)?;
    let [op, result, feature, _ty, mh, ml, ch, cl] = body else {
        return Err(opcode_or_length(body, GET_VCP_REPLY));
    };
    if *op != GET_VCP_REPLY {
        return Err(ReplyError::WrongOpcode(*op));
    }
    if *result != 0 {
        return Err(ReplyError::Unsupported);
    }
    if *feature != vcp {
        return Err(ReplyError::WrongFeature(*feature));
    }
    Ok(VcpValue {
        current: u16::from_be_bytes([*ch, *cl]),
        max: u16::from_be_bytes([*mh, *ml]),
    })
}

/// Returns the capabilities bytes in this chunk. An empty chunk ends the string.
pub fn decode_capabilities_reply(reply: &[u8], offset: u16) -> Result<&[u8], ReplyError> {
    let body = payload(reply)?;
    let [op, hi, lo, data @ ..] = body else {
        return Err(opcode_or_length(body, CAPS_REPLY));
    };
    if *op != CAPS_REPLY {
        return Err(ReplyError::WrongOpcode(*op));
    }
    let got = u16::from_be_bytes([*hi, *lo]);
    if got != offset {
        return Err(ReplyError::WrongOffset(got));
    }
    Ok(data)
}

/// Adds the length byte and checksum to a request body.
fn request(body: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(body.len() + 2);
    pkt.push(0x80 | body.len() as u8);
    pkt.extend_from_slice(body);
    let ck = pkt.iter().fold(DISPLAY ^ HOST, |a, b| a ^ b);
    pkt.push(ck);
    pkt
}

/// Validates the envelope of a reply and returns its body.
fn payload(reply: &[u8]) -> Result<&[u8], ReplyError> {
    let (&src, rest) = reply.split_first().ok_or(ReplyError::BadLength)?;
    if src != DISPLAY {
        return Err(ReplyError::BadSource(src));
    }
    let &len_byte = rest.first().ok_or(ReplyError::BadLength)?;
    let len = (len_byte & 0x7F) as usize;
    if len_byte & 0x80 == 0 || reply.len() < len + 3 {
        return Err(ReplyError::BadLength);
    }
    let ck = reply[..len + 2].iter().fold(REPLY_SEED, |a, b| a ^ b);
    if ck != reply[len + 2] {
        return Err(ReplyError::BadChecksum);
    }
    if len == 0 {
        return Err(ReplyError::Null);
    }
    Ok(&reply[2..len + 2])
}

fn opcode_or_length(body: &[u8], expected: u8) -> ReplyError {
    match body.first() {
        Some(&op) if op != expected => ReplyError::WrongOpcode(op),
        _ => ReplyError::BadLength,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_vcp_request_for_input_source() {
        assert_eq!(get_vcp_request(0x60), vec![0x82, 0x01, 0x60, 0xDC]);
    }

    #[test]
    fn set_vcp_request_for_hdmi() {
        assert_eq!(
            set_vcp_request(0x60, 0x11),
            vec![0x84, 0x03, 0x60, 0x00, 0x11, 0xC9]
        );
    }

    #[test]
    fn capabilities_request_at_offset_zero() {
        assert_eq!(capabilities_request(0), vec![0x83, 0xF3, 0x00, 0x00, 0x4F]);
    }

    #[test]
    fn decodes_input_source_reply_on_usb_c() {
        let reply = [
            0x6E, 0x88, 0x02, 0x00, 0x60, 0x00, 0x1B, 0x1B, 0x1B, 0x1B, 0xD4, 0x00,
        ];
        let v = decode_vcp_reply(&reply, 0x60).unwrap();
        assert_eq!(
            v,
            VcpValue {
                current: 0x1B1B,
                max: 0x1B1B
            }
        );
        assert_eq!(v.low_byte(), 27);
    }

    #[test]
    fn null_reply_after_a_switch_is_reported_as_null() {
        let reply = [
            0x6E, 0x80, 0xBE, 0x6E, 0x80, 0xBE, 0x6E, 0x80, 0xBE, 0x6E, 0x80, 0xBE,
        ];
        assert_eq!(decode_vcp_reply(&reply, 0x60), Err(ReplyError::Null));
    }

    #[test]
    fn reply_for_another_feature_is_rejected() {
        // Real stale reply seen during the spike: valid checksum, feature 0xDD.
        let reply = [
            0x6E, 0x88, 0x02, 0x00, 0xDD, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x69, 0x6E,
        ];
        assert_eq!(
            decode_vcp_reply(&reply, 0x60),
            Err(ReplyError::WrongFeature(0xDD))
        );
    }

    #[test]
    fn corrupted_reply_fails_the_checksum() {
        let reply = [
            0x6E, 0x88, 0x02, 0x00, 0x60, 0x00, 0x1B, 0x1B, 0x1B, 0x11, 0xD4, 0x00,
        ];
        assert_eq!(decode_vcp_reply(&reply, 0x60), Err(ReplyError::BadChecksum));
    }

    #[test]
    fn reply_read_one_byte_late_is_rejected_not_misread() {
        // m1ddc's "110" (0x6E) comes from reading at the wrong offset.
        let reply = [
            0x88, 0x02, 0x00, 0x60, 0x00, 0x1B, 0x1B, 0x1B, 0x1B, 0xD4, 0x00, 0x00,
        ];
        assert_eq!(
            decode_vcp_reply(&reply, 0x60),
            Err(ReplyError::BadSource(0x88))
        );
    }

    #[test]
    fn unsupported_feature_result_code_is_reported() {
        // Result code 0x01; checksum 0x50^6E^88^02^01^60 = 0xD5.
        let reply = [
            0x6E, 0x88, 0x02, 0x01, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0xD5, 0x00,
        ];
        assert_eq!(decode_vcp_reply(&reply, 0x60), Err(ReplyError::Unsupported));
    }

    #[test]
    fn decodes_capabilities_chunk() {
        let reply = [0x6E, 0x84, 0xE3, 0x00, 0x00, b'(', 0x71];
        assert_eq!(decode_capabilities_reply(&reply, 0).unwrap(), b"(");
    }

    #[test]
    fn empty_capabilities_chunk_marks_the_end() {
        let reply = [0x6E, 0x83, 0xE3, 0x00, 0x20, 0x7E];
        assert_eq!(decode_capabilities_reply(&reply, 0x20).unwrap(), b"");
    }

    #[test]
    fn capabilities_chunk_for_another_offset_is_rejected() {
        let reply = [0x6E, 0x83, 0xE3, 0x00, 0x20, 0x7E];
        assert_eq!(
            decode_capabilities_reply(&reply, 0),
            Err(ReplyError::WrongOffset(0x20))
        );
    }
}
