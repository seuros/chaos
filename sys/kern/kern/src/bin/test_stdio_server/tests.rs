use super::parse_data_url;
use pretty_assertions::assert_eq;

#[test]
fn parses_image_data_url() {
    let (mime, payload) = parse_data_url("data:image/png;base64,Zm9v").expect("parse data URL");
    assert_eq!(mime, "image/png");
    assert_eq!(payload, "Zm9v");
}
