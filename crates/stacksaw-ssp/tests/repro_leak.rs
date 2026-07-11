#![allow(unused_imports)]
use bytes::{BufMut, BytesMut};
use stacksaw_ssp::codec::{ContentLengthCodec};
use tokio_util::codec::Decoder;

#[test]
fn test_codec_header_growth_bounded() {
    let mut codec = ContentLengthCodec::new();
    let mut buf = BytesMut::new();
    
    // Feed many header lines without a terminator.
    let header_line = "X-Extra-Header: some-value\r\n";
    let mut err_count = 0;
    let mut iterations = 0;
    for _ in 0..10000 {
        buf.put_slice(header_line.as_bytes());
        iterations += 1;
        match codec.decode(&mut buf) {
            Err(e) => {
                assert!(e.to_string().contains("headers exceed maximum size"));
                err_count += 1;
                break;
            }
            Ok(None) => {}
            Ok(Some(_)) => panic!("expected no message"),
        }
    }
    
    assert_eq!(err_count, 1, "Expected codec to return error");
    assert!(iterations < 2400, "Should have failed earlier, took {} iterations", iterations);
    assert!(buf.len() < 2400 * header_line.len());
}
