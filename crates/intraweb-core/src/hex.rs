//! Minimal hex codec. A dependency-free stand-in so the binary stays small.

/// Encode bytes as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode lowercase or uppercase hex into exactly `N` bytes.
pub fn decode_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.as_bytes();
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = nibble(s[i * 2])?;
        let lo = nibble(s[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = [0x00u8, 0x0f, 0xa1, 0xff];
        assert_eq!(encode(&bytes), "000fa1ff");
        assert_eq!(decode_array::<4>("000fa1ff"), Some(bytes));
        assert_eq!(decode_array::<4>("000FA1FF"), Some(bytes));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode_array::<4>("000fa1"), None, "wrong length");
        assert_eq!(decode_array::<4>("000fa1gg"), None, "non-hex digit");
    }
}
