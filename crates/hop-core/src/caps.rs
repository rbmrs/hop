//! Parses a DDC/CI capabilities string into the monitor's input ports.

/// VCP feature code for the input source.
pub const INPUT_SOURCE: u8 = 0x60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPort {
    /// VCP 0x60 value that selects this port.
    pub code: u8,
    /// Detected name, e.g. "HDMI 1".
    pub name: String,
}

/// Returns the input ports listed under VCP 0x60, in the monitor's order.
/// Returns an empty list when the string has no usable 0x60 entry.
pub fn input_ports(caps: &str) -> Vec<InputPort> {
    let Some(vcp) = section(caps, "vcp") else {
        return Vec::new();
    };
    features(vcp)
        .into_iter()
        .find(|(code, _)| *code == INPUT_SOURCE)
        .and_then(|(_, values)| values)
        .map(|values| {
            hex_codes(values)
                .map(|code| InputPort {
                    code,
                    name: port_name(code),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Standard MCCS names for VCP 0x60 values. 0x1B is not in MCCS 2.1, but
/// Dell and LG use it for USB-C.
pub const STANDARD_INPUTS: &[(u8, &str)] = &[
    (0x01, "VGA 1"),
    (0x02, "VGA 2"),
    (0x03, "DVI 1"),
    (0x04, "DVI 2"),
    (0x05, "Composite 1"),
    (0x06, "Composite 2"),
    (0x07, "S-Video 1"),
    (0x08, "S-Video 2"),
    (0x09, "Tuner 1"),
    (0x0A, "Tuner 2"),
    (0x0B, "Tuner 3"),
    (0x0C, "Component 1"),
    (0x0D, "Component 2"),
    (0x0E, "Component 3"),
    (0x0F, "DP 1"),
    (0x10, "DP 2"),
    (0x11, "HDMI 1"),
    (0x12, "HDMI 2"),
    (0x1B, "USB-C"),
];

/// The standard name for a VCP 0x60 value, or "Input 0xNN".
pub fn port_name(code: u8) -> String {
    STANDARD_INPUTS
        .iter()
        .find(|(c, _)| *c == code)
        .map_or_else(
            || format!("Input 0x{code:02X}"),
            |(_, name)| name.to_string(),
        )
}

/// Parses a VCP 0x60 input code written in decimal ("17") or hex ("0x11").
/// 0 is not a valid input.
pub fn parse_code(text: &str) -> Option<u8> {
    let t = text.trim();
    let code = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u8::from_str_radix(hex, 16).ok()?,
        None => t.parse().ok()?,
    };
    (code != 0).then_some(code)
}

/// Content of the top-level `name(...)` group, without its parentheses.
fn section<'a>(caps: &'a str, name: &str) -> Option<&'a str> {
    let inner = caps.trim().strip_prefix('(').unwrap_or(caps);
    let mut depth = 0usize;
    let mut word_start = 0;
    for (i, c) in inner.char_indices() {
        match c {
            '(' => {
                if depth == 0 && inner[word_start..i].trim() == name {
                    return group_body(&inner[i..]);
                }
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    word_start = i + 1;
                }
            }
            _ => {}
        }
    }
    None
}

/// Body of the group that starts at `s[0] == '('`, up to its matching ')'.
fn group_body(s: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits a vcp body into (feature code, optional value list body).
fn features(vcp: &str) -> Vec<(u8, Option<&str>)> {
    let mut out = Vec::new();
    let mut rest = vcp;
    loop {
        rest = rest.trim_start();
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        if end == 0 {
            if rest.starts_with('(') {
                // Value list with no code in front: skip it.
                let body = group_body(rest).unwrap_or(rest);
                rest = &rest[(body.len() + 2).min(rest.len())..];
                continue;
            }
            return out;
        }
        let code = u8::from_str_radix(&rest[..end], 16).ok();
        rest = &rest[end..];
        let values = if rest.starts_with('(') {
            let body = group_body(rest);
            rest = &rest[body.map_or(rest.len(), |b| b.len() + 2)..];
            body
        } else {
            None
        };
        if let Some(code) = code {
            out.push((code, values));
        }
    }
}

fn hex_codes(s: &str) -> impl Iterator<Item = u8> + '_ {
    s.split_whitespace()
        .filter_map(|t| u8::from_str_radix(t, 16).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const U3223QE: &str = "(prot(monitor)type(LCD)model(U3223QE)cmds(01 02 03 07 0C E3 F3)vcp(02 04 05 08 10 12 14(01 04 05 06 08 09 0B 0C) 16 18 1A 52 60(1B 0F 11 ) AA(01 02 04 ) AC AE B2 B6 C6 C8 C9 CA CC(02 0A 03 04 08 09 0D 06 ) D6(01 04 05) DC(00 03 05 ) DF E0 E1 E2(00 02 04 0C 0D 0F 10 11 13 0B 1A 1B 14 27 23 24 3A ) E5 E7(02 03) E8 E9(00 01 02 21 22 24 ) EA F0(09 0A 31 32 34 36 ) EF F1 F2 FD)mswhql(1)asset_eep(40)mccs_ver(2.1))";

    #[test]
    fn dell_u3223qe_lists_usb_c_dp_and_hdmi() {
        let ports = input_ports(U3223QE);
        let got: Vec<(u8, &str)> = ports.iter().map(|p| (p.code, p.name.as_str())).collect();
        assert_eq!(got, vec![(27, "USB-C"), (15, "DP 1"), (17, "HDMI 1")]);
    }

    #[test]
    fn no_input_source_feature_gives_no_ports() {
        assert!(input_ports("(prot(monitor)vcp(10 12 14(05 08)))").is_empty());
        assert!(input_ports("garbage").is_empty());
    }

    #[test]
    fn vcp_60_inside_another_feature_list_is_not_the_input_source() {
        let ports = input_ports("(vcp(14(60 01) 60(11 12)))");
        let codes: Vec<u8> = ports.iter().map(|p| p.code).collect();
        assert_eq!(codes, vec![0x11, 0x12]);
    }

    #[test]
    fn unknown_codes_get_a_hex_name() {
        let ports = input_ports("(vcp(60(0F 31)))");
        assert_eq!(ports[1].name, "Input 0x31");
    }

    #[test]
    fn parses_decimal_and_hex_input_codes() {
        assert_eq!(parse_code("17"), Some(17));
        assert_eq!(parse_code(" 0x11 "), Some(17));
        assert_eq!(parse_code("0X1b"), Some(27));
        assert_eq!(parse_code("255"), Some(255));
    }

    #[test]
    fn rejects_zero_out_of_range_and_text() {
        for bad in ["0", "0x0", "256", "0x100", "-1", "hdmi", ""] {
            assert_eq!(parse_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn standard_inputs_list_the_mccs_names() {
        assert!(STANDARD_INPUTS.contains(&(0x11, "HDMI 1")));
        assert!(STANDARD_INPUTS.contains(&(0x1B, "USB-C")));
    }
}
