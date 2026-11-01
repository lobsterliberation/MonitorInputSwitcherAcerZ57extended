//! VESA DDC/CI 1.1 packet framing, shared by the backends that talk raw I2C (Linux i2c-dev,
//! macOS IOAVService). Windows goes through dxva2, which does this itself.
//!
//! Protocol (VESA DDC/CI 1.1):
//! - Set VCP:  51 84 03 <vcp> <hi> <lo> <chk>        chk = 0x6E ^ xor(bytes)
//! - Get VCP:  51 82 01 <vcp> <chk>                  then wait 40 ms and read 11 bytes:
//!   6E 88 02 <result> <vcp> <type> <max hi> <max lo> <cur hi> <cur lo> <chk>   chk = 0x50 ^ xor(bytes)
//! - Capabilities: 51 83 F3 <offset hi> <offset lo> <chk>, then read fragments
//!   6E <0x80|len> E3 <offset hi> <offset lo> <data...> <chk> until an empty one. The text lists the
//!   supported features, e.g. "vcp(... 60(0F 11 1B) ...)" = the accepted input codes.

use std::time::Duration;

/// The monitor needs this long between a request and reading its reply...
pub const REPLY_DELAY: Duration = Duration::from_millis(40);
/// ...and between two commands.
pub const COMMAND_GAP: Duration = Duration::from_millis(50);
pub const TRIES: usize = 3;
/// Capabilities fragments need a little longer than a VCP reply.
pub const CAPS_DELAY: Duration = Duration::from_millis(50);
pub const CAPS_MAX_LEN: usize = 4096;

pub fn xor(bytes: &[u8], init: u8) -> u8 {
    bytes.iter().fold(init, |acc, b| acc ^ b)
}

pub fn set_vcp_packet(vcp: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let msg = [0x51, 0x84, 0x03, vcp, hi, lo];
    let mut out = [0u8; 7];
    out[..6].copy_from_slice(&msg);
    out[6] = xor(&msg, 0x6E);
    out
}

pub fn get_vcp_packet(vcp: u8) -> [u8; 5] {
    let msg = [0x51, 0x82, 0x01, vcp];
    [msg[0], msg[1], msg[2], msg[3], xor(&msg, 0x6E)]
}

pub fn caps_request_packet(offset: u16) -> [u8; 6] {
    let [hi, lo] = offset.to_be_bytes();
    let msg = [0x51, 0x83, 0xF3, hi, lo];
    [msg[0], msg[1], msg[2], msg[3], msg[4], xor(&msg, 0x6E)]
}

/// Parses one capabilities reply fragment into (offset, data).
pub fn parse_caps_fragment(r: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let len = usize::from(r.get(1).copied().unwrap_or(0) & 0x7F); // opcode + offset + data
    if len < 3 || r.len() < len + 3 || r[2] != 0xE3 {
        return Err(format!(
            "Unexpected capabilities reply: {}",
            hex(&r[..r.len().min(8)])
        ));
    }
    if xor(&r[..len + 2], 0x50) != r[len + 2] {
        return Err("Capabilities reply checksum mismatch".into());
    }
    Ok((u16::from_be_bytes([r[3], r[4]]), r[5..len + 2].to_vec()))
}

/// Parses a "VCP feature reply"; returns the current value (high byte included).
pub fn parse_vcp_reply(vcp: u8, r: &[u8]) -> Result<u32, String> {
    if r.len() < 11 || r[1] & 0x7F == 0 {
        // 6E 80 BE is the "null message": the monitor is busy.
        return Err("The monitor sent an empty reply".into());
    }
    if r[1] != 0x88 || r[2] != 0x02 || r[4] != vcp {
        return Err(format!("Unexpected DDC reply: {}", hex(r)));
    }
    if xor(&r[..10], 0x50) != r[10] {
        return Err(format!("DDC reply checksum mismatch: {}", hex(r)));
    }
    if r[3] != 0 {
        return Err(format!("The monitor does not support VCP 0x{vcp:02X}"));
    }
    Ok(u32::from(u16::from_be_bytes([r[8], r[9]])))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
