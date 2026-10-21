use crate::admission::{header_value, percent_decode};
use crate::transport::write_ws_text;

#[test]
fn percent_decode_directly_covers_encoded_text_plus_and_malformed_bytes() {
    assert_eq!(percent_decode("A%20B+%E4%B8%AD"), Ok("A B 中".to_string()));
    for malformed in ["%", "%0", "%GG", "%FF"] {
        assert_eq!(percent_decode(malformed), Err(()), "input={malformed:?}");
    }
}

#[test]
fn header_lookup_matches_ascii_case_insensitively_and_ignores_body() {
    let request = "GET /health HTTP/1.1\r\ncOnTeNt-TyPe:  application/json \r\nX-Test: value\r\n\r\nContent-Type: forged";
    assert_eq!(
        header_value(request, "content-type"),
        Some("application/json".to_string())
    );
    assert_eq!(header_value(request, "x-TEST"), Some("value".to_string()));
    assert_eq!(header_value(request, "missing"), None);
}

#[test]
fn websocket_text_writer_encodes_all_three_payload_length_forms() {
    let mut short = Vec::new();
    write_ws_text(&mut short, &"s".repeat(125)).expect("write 7-bit length frame");
    assert_eq!(&short[..2], &[0x81, 125]);
    assert_eq!(short.len(), 127);

    let mut medium = Vec::new();
    write_ws_text(&mut medium, &"m".repeat(126)).expect("write 16-bit length frame");
    assert_eq!(&medium[..4], &[0x81, 126, 0, 126]);
    assert_eq!(medium.len(), 130);

    let mut long = Vec::new();
    write_ws_text(&mut long, &"l".repeat(65_536)).expect("write 64-bit length frame");
    assert_eq!(&long[..10], &[0x81, 127, 0, 0, 0, 0, 0, 1, 0, 0]);
    assert_eq!(long.len(), 65_546);
}
