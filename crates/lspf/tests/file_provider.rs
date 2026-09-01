use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use lspf::types::Uri;
use lspf::{
    MemoryFileProvider, OsFileProvider, RawMessage, RequestId, Server, ServerContext, Transport,
    TransportError, TransportReader, TransportWriter,
};
use tokio::sync::mpsc;

fn uri(spelling: &str) -> Uri {
    Uri::from_str(spelling).expect("the test URI parses")
}

/// An absolute `file:` URI for `path`, percent-encoding every byte that is
/// not legal in a URI path.
fn file_uri(path: &Path) -> Uri {
    let absolute = std::path::absolute(path).expect("the test path is absolute");
    let text = absolute.to_str().expect("test paths are valid UTF-8");
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    // On Windows the drive letter is part of the path, so the URI needs the
    // empty authority spelled out as a third slash: `file:///C:/…`.
    #[cfg(windows)]
    let spelling = format!("file:///{text}");
    #[cfg(not(windows))]
    let spelling = format!("file://{text}");
    uri(&percent_encode(&spelling))
}

fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The provider contract every [`FileProvider`](lspf::FileProvider) must
/// keep, expressed as shared assertions both [`MemoryFileProvider`] and
/// [`OsFileProvider`] exercise: a missing resource, percent-encoded URI
/// equivalence, and the raw read result. Coverage that legitimately differs
/// per provider — the meaning of non-file URIs — lives in each provider's
/// contract module alongside its platform-specific Windows-path coverage.
mod provider_contract {
    use lspf::FileProvider;
    use lspf::types::Uri;

    /// A resource the provider does not hold reads as `Ok(None)`, which the
    /// workspace reports as `WorkspaceError::NotFound`.
    pub(super) async fn missing_is_none(provider: &impl FileProvider, uri: Uri) {
        assert_eq!(provider.read_text(&uri).await.unwrap(), None);
    }

    /// Percent-encoded spellings of one URI name one resource.
    pub(super) async fn percent_encoded_spellings_read_one_text(
        provider: &impl FileProvider,
        encoded: Uri,
        plain: Uri,
        expected: &str,
    ) {
        assert_eq!(
            provider.read_text(&encoded).await.unwrap().as_deref(),
            Some(expected)
        );
        assert_eq!(
            provider.read_text(&plain).await.unwrap().as_deref(),
            Some(expected)
        );
    }

    /// One read returns the resource's current text. Callers prove reads are
    /// never cached by restaging the resource between two calls.
    pub(super) async fn reads_return(provider: &impl FileProvider, uri: &Uri, expected: &str) {
        assert_eq!(
            provider.read_text(uri).await.unwrap().as_deref(),
            Some(expected)
        );
    }
}

mod memory_contract {
    use lspf::{FileProvider, MemoryFileProvider};

    use super::{provider_contract, uri};

    #[tokio::test]
    async fn memory_provider_clones_share_normalized_uri_entries_and_removals() {
        let provider = MemoryFileProvider::new();
        let clone = provider.clone();
        let encoded = uri("file:///workspace/%61.rs");
        let plain = uri("FILE:///workspace/a.rs");

        provider.insert(encoded, "first");

        assert_eq!(
            clone.read_text(&plain).await.unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(clone.remove(&plain).as_deref(), Some("first"));
        assert_eq!(provider.read_text(&plain).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_missing_resource_is_none() {
        provider_contract::missing_is_none(
            &MemoryFileProvider::new(),
            uri("file:///workspace/absent.rs"),
        )
        .await;
    }

    #[tokio::test]
    async fn percent_encoded_spellings_share_one_entry() {
        let provider = MemoryFileProvider::new();
        provider.insert(uri("file:///workspace/%61.rs"), "percent");
        provider_contract::percent_encoded_spellings_read_one_text(
            &provider,
            uri("file:///workspace/%61.rs"),
            uri("file:///workspace/a.rs"),
            "percent",
        )
        .await;
    }

    #[tokio::test]
    async fn reads_are_not_cached() {
        let provider = MemoryFileProvider::new();
        let requested = uri("file:///workspace/live.rs");
        provider.insert(requested.clone(), "one");
        provider_contract::reads_return(&provider, &requested, "one").await;
        provider.insert(requested.clone(), "two");
        provider_contract::reads_return(&provider, &requested, "two").await;
    }

    #[tokio::test]
    async fn non_file_uris_are_ordinary_entries() {
        let provider = MemoryFileProvider::new();
        let requested = uri("untitled:///scratch.rs");
        provider.insert(requested.clone(), "virtual");
        assert_eq!(
            provider.read_text(&requested).await.unwrap().as_deref(),
            Some("virtual")
        );
    }
}

mod os_contract {
    use lspf::{FileProvider, OsFileProvider, WorkspaceError};

    use super::{file_uri, provider_contract, uri};

    #[tokio::test]
    async fn a_missing_file_is_none() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        provider_contract::missing_is_none(
            &OsFileProvider::new(),
            file_uri(&dir.path().join("absent.rs")),
        )
        .await;
    }

    #[tokio::test]
    async fn percent_encoded_spellings_read_one_file() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        std::fs::write(dir.path().join("a.rs"), "percent").expect("the test file writes");
        let dir_spelling = file_uri(dir.path()).as_str().to_string();
        provider_contract::percent_encoded_spellings_read_one_text(
            &OsFileProvider::new(),
            uri(&format!("{dir_spelling}/%61.rs")),
            uri(&format!("{dir_spelling}/a.rs")),
            "percent",
        )
        .await;
    }

    #[tokio::test]
    async fn reads_are_not_cached() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let file = dir.path().join("live.rs");
        std::fs::write(&file, "one").expect("the test file writes");
        let requested = file_uri(&file);
        let provider = OsFileProvider::new();
        provider_contract::reads_return(&provider, &requested, "one").await;
        std::fs::write(&file, "two").expect("the test file rewrites");
        provider_contract::reads_return(&provider, &requested, "two").await;
    }

    #[tokio::test]
    async fn rejects_every_non_file_scheme() {
        let provider = OsFileProvider::new();
        for spelling in [
            "untitled:Untitled-1",
            "untitled:///x.rs",
            "http://example.com/a.rs",
        ] {
            let parsed = uri(spelling);
            let expected_scheme = parsed.scheme().as_str();
            let err = provider.read_text(&parsed).await.unwrap_err();
            assert!(
                matches!(err, WorkspaceError::UnsupportedScheme(ref scheme) if scheme == expected_scheme),
                "{spelling} should be unsupported, got {err:?}"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn drive_letter_spellings_read_one_windows_file() {
        let dir = tempfile::tempdir().expect("a tempdir is created");
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "drive").expect("the test file writes");
        let text = std::path::absolute(&file).unwrap();
        let text = text.to_str().unwrap().replace('\\', "/");
        let (letter, tail) = text.split_at(1);
        let rest = &tail[1..];
        let provider = OsFileProvider::new();
        for spelling in [
            format!("file:///{letter}{tail}"),
            format!("file:///{}%3A{rest}", letter.to_lowercase()),
            format!("file:///{}%3a{rest}", letter.to_lowercase()),
        ] {
            assert_eq!(
                provider
                    .read_text(&uri(&spelling))
                    .await
                    .unwrap()
                    .as_deref(),
                Some("drive"),
                "{spelling}"
            );
        }
    }
}

struct ChannelTransport {
    input: mpsc::UnboundedReceiver<RawMessage>,
    output: mpsc::UnboundedSender<RawMessage>,
}

struct ChannelReader(mpsc::UnboundedReceiver<RawMessage>);
struct ChannelWriter(mpsc::UnboundedSender<RawMessage>);

impl Transport for ChannelTransport {
    type Reader = ChannelReader;
    type Writer = ChannelWriter;

    fn split(self) -> (Self::Reader, Self::Writer) {
        (ChannelReader(self.input), ChannelWriter(self.output))
    }
}

impl TransportReader for ChannelReader {
    async fn recv(&mut self) -> Result<RawMessage, TransportError> {
        self.0.recv().await.ok_or(TransportError::Closed)
    }
}

impl TransportWriter for ChannelWriter {
    async fn send(&mut self, message: RawMessage) -> Result<(), TransportError> {
        self.0.send(message).map_err(|_| TransportError::Closed)
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::test]
async fn builder_provider_is_used_by_the_established_workspace() {
    let provider = MemoryFileProvider::new();
    let requested = uri("file:///workspace/unopened.rs");
    provider.insert(requested.clone(), "configured provider");
    let observed = Arc::new(Mutex::new(None));
    let hook_observed = Arc::clone(&observed);
    let hook_uri = requested.clone();
    let server = Server::builder(())
        .file_provider(provider)
        .on_initialize(move |_state, ctx: ServerContext, _params, _ct| {
            let observed = Arc::clone(&hook_observed);
            let uri = hook_uri.clone();
            async move {
                let text = ctx.workspace().text_document(&uri).await.unwrap().text();
                *observed.lock().unwrap() = Some(text);
                Ok(None)
            }
        })
        .build()
        .unwrap();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(server.serve(ChannelTransport {
        input: input_rx,
        output: output_tx,
    }));

    input_tx
        .send(RawMessage::Request {
            id: RequestId::Number(1),
            method: "initialize".into(),
            params: Bytes::from_static(br#"{"processId":null,"rootUri":null,"capabilities":{}}"#),
        })
        .unwrap();
    output_rx.recv().await.expect("initialize response");
    input_tx
        .send(RawMessage::Notification {
            method: "exit".into(),
            params: Bytes::from_static(b"null"),
        })
        .unwrap();
    drop(input_tx);
    handle.await.unwrap().unwrap();

    assert_eq!(
        observed.lock().unwrap().as_deref(),
        Some("configured provider")
    );
}

#[tokio::test]
async fn builder_os_provider_serves_unopened_files_outside_any_root() {
    let dir = tempfile::tempdir().expect("a tempdir is created");
    let file = dir.path().join("unopened.rs");
    std::fs::write(&file, "os provider file").expect("the test file writes");
    let requested = file_uri(&file);
    let observed = Arc::new(Mutex::new(None));
    let hook_observed = Arc::clone(&observed);
    let hook_uri = requested.clone();
    // `rootUri` is null, so the workspace has no roots at all: a readable
    // file proves the provider does not restrict reads to workspace roots.
    let server = Server::builder(())
        .file_provider(OsFileProvider::new())
        .on_initialize(move |_state, ctx: ServerContext, _params, _ct| {
            let observed = Arc::clone(&hook_observed);
            let uri = hook_uri.clone();
            async move {
                let text = ctx.workspace().text_document(&uri).await.unwrap().text();
                *observed.lock().unwrap() = Some(text);
                Ok(None)
            }
        })
        .build()
        .unwrap();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(server.serve(ChannelTransport {
        input: input_rx,
        output: output_tx,
    }));

    input_tx
        .send(RawMessage::Request {
            id: RequestId::Number(1),
            method: "initialize".into(),
            params: Bytes::from_static(br#"{"processId":null,"rootUri":null,"capabilities":{}}"#),
        })
        .unwrap();
    output_rx.recv().await.expect("initialize response");
    input_tx
        .send(RawMessage::Notification {
            method: "exit".into(),
            params: Bytes::from_static(b"null"),
        })
        .unwrap();
    drop(input_tx);
    handle.await.unwrap().unwrap();

    assert_eq!(
        observed.lock().unwrap().as_deref(),
        Some("os provider file")
    );
}
