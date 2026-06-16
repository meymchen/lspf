use lspf::types::{GeneralClientCapabilities, InitializeParams, PositionEncodingKind};
use lspf::{Documents, LanguageServer, PositionEncoding};

struct TestServer {
    documents: Documents,
}

impl LanguageServer for TestServer {
    fn documents(&self) -> &Documents {
        &self.documents
    }
}

#[test]
fn defaults_to_utf16_when_client_offers_no_encodings() {
    let server = TestServer {
        documents: Documents::new(),
    };
    let params = InitializeParams::default();

    let caps = server.server_capabilities(&params);

    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF16));
    assert_eq!(
        server.documents().position_encoding(),
        PositionEncoding::Utf16
    );
}

#[test]
fn picks_utf8_when_client_offers_it() {
    let server = TestServer {
        documents: Documents::new(),
    };
    let mut params = InitializeParams::default();
    params.capabilities.general = Some(GeneralClientCapabilities {
        position_encodings: Some(vec![PositionEncodingKind::UTF8]),
        ..GeneralClientCapabilities::default()
    });

    let caps = server.server_capabilities(&params);

    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF8));
    assert_eq!(
        server.documents().position_encoding(),
        PositionEncoding::Utf8
    );
}

#[test]
fn falls_back_to_utf16_when_client_only_offers_utf16() {
    let server = TestServer {
        documents: Documents::new(),
    };
    let mut params = InitializeParams::default();
    params.capabilities.general = Some(GeneralClientCapabilities {
        position_encodings: Some(vec![PositionEncodingKind::UTF16]),
        ..GeneralClientCapabilities::default()
    });

    let caps = server.server_capabilities(&params);

    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF16));
    assert_eq!(
        server.documents().position_encoding(),
        PositionEncoding::Utf16
    );
}

#[test]
fn falls_back_to_utf16_when_client_offers_only_unsupported_encodings() {
    let server = TestServer {
        documents: Documents::new(),
    };
    let mut params = InitializeParams::default();
    params.capabilities.general = Some(GeneralClientCapabilities {
        position_encodings: Some(vec![PositionEncodingKind::UTF32]),
        ..GeneralClientCapabilities::default()
    });

    let caps = server.server_capabilities(&params);

    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF16));
    assert_eq!(
        server.documents().position_encoding(),
        PositionEncoding::Utf16
    );
}

#[test]
fn prefers_utf8_over_utf16_when_client_offers_both() {
    let server = TestServer {
        documents: Documents::new(),
    };
    let mut params = InitializeParams::default();
    params.capabilities.general = Some(GeneralClientCapabilities {
        position_encodings: Some(vec![
            PositionEncodingKind::UTF16,
            PositionEncodingKind::UTF8,
        ]),
        ..GeneralClientCapabilities::default()
    });

    let caps = server.server_capabilities(&params);

    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF8));
    assert_eq!(
        server.documents().position_encoding(),
        PositionEncoding::Utf8
    );
}
