use super::{respond_to_client_frames, WsCloseScanner};

fn masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let mut frame = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    frame
}

#[test]
fn client_ping_gets_an_unmasked_pong_with_the_same_payload() {
    let mut scanner = WsCloseScanner::new();
    let ping = masked_frame(0x9, b"probe");
    let mut response = Vec::new();
    assert!(!respond_to_client_frames(&mut response, scanner.push(&ping)).unwrap());
    assert_eq!(response, [0x8a, 5, b'p', b'r', b'o', b'b', b'e']);
}

#[test]
fn split_ping_waits_for_its_complete_payload_before_pong() {
    let mut scanner = WsCloseScanner::new();
    let ping = masked_frame(0x9, b"split");
    let mut response = Vec::new();
    assert!(!respond_to_client_frames(&mut response, scanner.push(&ping[..4])).unwrap());
    assert!(response.is_empty());
    assert!(!respond_to_client_frames(&mut response, scanner.push(&ping[4..])).unwrap());
    assert_eq!(response, [0x8a, 5, b's', b'p', b'l', b'i', b't']);
}

#[test]
fn ping_before_close_in_one_read_is_answered_before_session_ends() {
    let mut scanner = WsCloseScanner::new();
    let mut frames = masked_frame(0x9, b"bye");
    frames.extend(masked_frame(0x8, b""));
    let mut response = Vec::new();
    assert!(respond_to_client_frames(&mut response, scanner.push(&frames)).unwrap());
    assert_eq!(response, [0x8a, 3, b'b', b'y', b'e']);
}
