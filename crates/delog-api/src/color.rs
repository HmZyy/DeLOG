use crate::{Error, Result};

pub fn parse_hex_color(color: &str) -> Result<[f32; 4]> {
    let invalid = || Error::invalid_input("marker color must be #RRGGBB or #RRGGBBAA");
    let digits = color.as_bytes().strip_prefix(b"#").ok_or_else(invalid)?;
    if !matches!(digits.len(), 6 | 8) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }
    let parse_byte = |start| {
        let nibble = |digit| match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'f' => digit - b'a' + 10,
            b'A'..=b'F' => digit - b'A' + 10,
            _ => unreachable!("hex digits validated above"),
        };
        nibble(digits[start]) * 16 + nibble(digits[start + 1])
    };
    Ok([
        parse_byte(0) as f32 / 255.0,
        parse_byte(2) as f32 / 255.0,
        parse_byte(4) as f32 / 255.0,
        if digits.len() == 8 {
            parse_byte(6) as f32 / 255.0
        } else {
            1.0
        },
    ])
}

pub fn format_hex_color(color: [f32; 4]) -> String {
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        byte(color[0]),
        byte(color[1]),
        byte(color[2]),
        byte(color[3])
    )
}
