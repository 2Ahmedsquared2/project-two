//! Lowercase hex helpers.

pub(crate) const HEX: &[u8; 16] = b"0123456789abcdef";

/// Append `bytes` as lowercase hex.
pub(crate) fn write_hex(out: &mut String, bytes: &[u8]) {
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
}

/// `0x` + minimal hex of a big-endian integer, as `eth_utils.to_hex(int)` prints
/// r and s in the SDK's signature dict: leading zero nibbles are dropped, and zero
/// is `0x0`.
pub(crate) fn write_minimal_hex(out: &mut String, be: &[u8; 32]) {
    out.push_str("0x");
    let start = out.len();
    for b in be {
        for nib in [b >> 4, b & 0x0f] {
            if nib != 0 || out.len() > start {
                out.push(char::from(HEX[usize::from(nib)]));
            }
        }
    }
    if out.len() == start {
        out.push('0');
    }
}
