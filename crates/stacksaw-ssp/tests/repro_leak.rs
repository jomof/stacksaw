#![allow(unused_imports)]
use bytes::{BufMut, BytesMut};
use stacksaw_ssp::codec::{ContentLengthCodec};
use tokio_util::codec::Decoder;

#[test]
fn test_codec_header_growth_unbounded() {
    let mut codec = ContentLengthCodec::new();
    let mut buf = BytesMut::new();
    
    // Feed many header lines without a terminator.
    let header_line = "X-Extra-Header: some-value\r\n";
    for _ in 0..10000 {
        buf.put_slice(header_line.as_bytes());
        let _ = codec.decode(&mut buf);
    }
    
    assert!(buf.len() >= 10000 * header_line.len());
}
