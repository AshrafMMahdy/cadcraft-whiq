//! Binary DXF written by R12 and earlier: 1-byte group codes (255 escapes a 2-byte one).
#![allow(clippy::unwrap_used)]

use cadcraft_dxf::{Value, parse};

fn tag(out: &mut Vec<u8>, code: u8, s: &str) {
    out.push(code);
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

#[test]
fn r12_binary_dxf_reads_with_one_byte_group_codes() {
    let mut b = b"AutoCAD Binary DXF\r\n\x1a\0".to_vec();
    tag(&mut b, 0, "SECTION");
    tag(&mut b, 2, "HEADER");
    tag(&mut b, 9, "$ACADVER");
    tag(&mut b, 1, "AC1009");
    tag(&mut b, 9, "$LUNITS");
    b.push(70);
    b.extend_from_slice(&2i16.to_le_bytes());
    // An escaped 2-byte code (1071, a 32-bit integer).
    b.push(255);
    b.extend_from_slice(&1071i16.to_le_bytes());
    b.extend_from_slice(&7i32.to_le_bytes());
    tag(&mut b, 0, "ENDSEC");
    tag(&mut b, 0, "EOF");
    let tags = parse(&b).unwrap();
    let codes: Vec<i32> = tags.iter().map(|t| t.code).collect();
    assert_eq!(codes, vec![0, 2, 9, 1, 9, 70, 1071, 0, 0]);
    assert_eq!(tags[3].value, Value::Str("AC1009".into()));
    assert_eq!(tags[5].value, Value::Int(2));
    assert_eq!(tags[6].value, Value::Int(7));
}
