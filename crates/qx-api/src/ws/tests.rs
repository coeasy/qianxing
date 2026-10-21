use super::{WsCloseScanner, WS_CLOSE_PENDING_CAP};

fn masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 125);
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
fn an_unfinished_frame_cannot_grow_the_buffer_past_the_cap() {
    // Cross-read accumulation is a new memory surface the old byte scan did not have:
    // a client that dribbles bytes without ever completing a frame header would
    // otherwise grow `pending` without bound. Past the cap the connection is
    // reported as closed instead of accumulating forever.
    let mut scanner = WsCloseScanner::new();
    assert!(
        !scanner
            .push(&[0x81_u8, 0xff_u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
            .closed,
        "a 10-byte header with an 8-byte length is still incomplete"
    );
    assert_eq!(scanner.pending.len(), 10);
    assert!(
        scanner.push(&vec![0x18_u8; WS_CLOSE_PENDING_CAP]).closed,
        "overflowing the cap is treated as a close"
    );
    assert!(
        scanner.pending.is_empty(),
        "the buffer is drained, not grown"
    );
}

#[test]
fn payload_bytes_matching_close_are_not_a_close_frame() {
    // Text frame carrying [0x18, 0x00, 0x01]: every one of those bytes satisfies
    // the old `byte & 0x0f == 0x8` test, so the previous implementation closed
    // the connection on a perfectly ordinary data frame.
    let mut scanner = WsCloseScanner::new();
    assert!(
        !scanner.push(&masked_frame(0x1, &[0x18, 0x00, 0x01])).closed,
        "a data frame is not a close"
    );
    // And the very byte 0x68 ('h') also matches the old test.
    assert!(!scanner.push(&masked_frame(0x1, b"hello")).closed);
}

#[test]
fn a_complete_close_frame_is_detected() {
    let mut scanner = WsCloseScanner::new();
    assert!(
        scanner.push(&masked_frame(0x8, b"")).closed,
        "an empty close frame closes"
    );
    assert!(
        WsCloseScanner::new()
            .push(&masked_frame(0x8, &[0x03, 0xe8]))
            .closed,
        "a close frame with a status payload closes"
    );
}

#[test]
fn a_close_frame_split_across_reads_is_still_detected() {
    let mut scanner = WsCloseScanner::new();
    let frame = masked_frame(0x8, &[0x03, 0xe8]);
    assert!(!scanner.push(&frame[..4]).closed);
    assert!(
        scanner.push(&frame[4..]).closed,
        "the second half completes the frame"
    );
}

#[test]
fn a_data_frame_before_close_in_one_read_still_reaches_the_close() {
    // One read delivering a text frame then a close frame: the text frame is
    // skipped whole, the close is found at its own header.
    let mut scanner = WsCloseScanner::new();
    let mut frames = masked_frame(0x1, &[0x18, 0x28, 0x38]);
    frames.extend(masked_frame(0x8, b""));
    assert!(scanner.push(&frames).closed);
}

#[test]
fn an_extended_length_frame_is_skipped_whole() {
    // 128-byte payload: the 2-byte extended length form. The payload bytes all
    // match the old close test; the frame must be skipped by length, not scanned.
    let mut payload = [0_u8; 128];
    payload.iter_mut().for_each(|byte| *byte = 0x18);
    // Extended-length frames are validated as client frames too: set the mask
    // flag and reserve the four-byte mask key before the payload.
    let mut frame = vec![0x81_u8, 0xfe_u8, 0x00_u8, 0x80_u8, 0x12, 0x34, 0x56, 0x78];
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ [0x12_u8, 0x34, 0x56, 0x78][index % 4]),
    );
    let mut scanner = WsCloseScanner::new();
    assert!(
        !scanner.push(&frame).closed,
        "a 128-byte text frame is not a close"
    );
}

#[test]
fn an_unmasked_client_frame_ends_the_session() {
    assert!(
        WsCloseScanner::new().push(&[0x81, 0x00]).closed,
        "client frames without the mandatory mask bit are rejected"
    );
}
