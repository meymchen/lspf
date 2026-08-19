//! Replaceable text-resource lookup for unopened documents.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use lsp_types::Uri;

use crate::runtime::TaskSend;
use crate::uri_key::UriKey;
use crate::workspace::WorkspaceError;

/// Supplies text for resources that are not open in the editor.
///
/// A successful `Some` result becomes a version-less [`Document`] snapshot;
/// `Ok(None)` means the provider has no resource for the URI and becomes
/// [`WorkspaceError::NotFound`]. Providers report failures — an unsupported
/// scheme, unreadable bytes, or an I/O error — through [`WorkspaceError`], and
/// never cache: every lookup asks the provider again.
///
/// The returned future is `Send` on native targets and may remain local on
/// Worker-hosted WASM, matching the framework's runtime task boundary.
///
/// [`Document`]: crate::Document
pub trait FileProvider: Send + Sync + 'static {
    fn read_text(
        &self,
        uri: &Uri,
    ) -> impl Future<Output = Result<Option<String>, WorkspaceError>> + TaskSend;
}

#[cfg(not(target_arch = "wasm32"))]
type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, WorkspaceError>> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, WorkspaceError>> + 'a>>;

pub(crate) trait ErasedFileProvider: Send + Sync {
    fn read_text<'a>(&'a self, uri: &'a Uri) -> ProviderFuture<'a>;
}

struct FileProviderAdapter<P>(P);

impl<P: FileProvider> ErasedFileProvider for FileProviderAdapter<P> {
    fn read_text<'a>(&'a self, uri: &'a Uri) -> ProviderFuture<'a> {
        Box::pin(self.0.read_text(uri))
    }
}

pub(crate) type SharedFileProvider = Arc<dyn ErasedFileProvider>;

pub(crate) fn erase(provider: impl FileProvider) -> SharedFileProvider {
    Arc::new(FileProviderAdapter(provider))
}

/// An in-memory [`FileProvider`] intended for tests and virtual resources.
///
/// Clones share one backing store. URI lookup uses the same normalized
/// identity as the connection's open documents.
#[derive(Debug, Clone, Default)]
pub struct MemoryFileProvider {
    entries: Arc<RwLock<HashMap<UriKey, String>>>,
}

impl MemoryFileProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the text associated with `uri`.
    pub fn insert(&self, uri: impl Borrow<Uri>, text: impl Into<String>) -> Option<String> {
        self.entries
            .write()
            .unwrap()
            .insert(UriKey::new(uri.borrow()), text.into())
    }

    /// Remove and return the text associated with `uri`, if present.
    pub fn remove(&self, uri: &Uri) -> Option<String> {
        self.entries.write().unwrap().remove(&UriKey::new(uri))
    }
}

impl FileProvider for MemoryFileProvider {
    async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
        Ok(self.entries.read().unwrap().get(&UriKey::new(uri)).cloned())
    }
}

/// The empty [`FileProvider`] that serves no scheme at all.
///
/// Every lookup fails with [`WorkspaceError::UnsupportedScheme`], naming the
/// requested scheme. It is the default provider on Worker-hosted WASM, where
/// lspf assumes no host filesystem and compiles no native OS provider
/// (ADR 0020); the builder still accepts any user provider, including one
/// backed by host JavaScript APIs.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyFileProvider;

impl FileProvider for EmptyFileProvider {
    async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
        let scheme = uri
            .scheme()
            .map(|scheme| scheme.as_str())
            .unwrap_or_default();
        Err(WorkspaceError::UnsupportedScheme(scheme.to_string()))
    }
}

/// The connection's default [`FileProvider`]: an in-memory provider on native
/// targets, the empty provider on Worker-hosted WASM (ADR 0020).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn default_file_provider() -> SharedFileProvider {
    erase(MemoryFileProvider::new())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn default_file_provider() -> SharedFileProvider {
    erase(EmptyFileProvider)
}

/// The production `FileProvider` adapter that reads `file:` URIs from the
/// local filesystem (issue #80). Not compiled on Worker-hosted WASM; its tokio
/// I/O stays inside this native-only boundary (ADR 0020).
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
mod os {
    use std::path::PathBuf;

    use lsp_types::Uri;
    use tokio::io::AsyncReadExt;

    use super::FileProvider;
    use crate::uri_key::percent_decode_strict;
    use crate::workspace::WorkspaceError;

    /// The default [`FileProvider`] that reads `file:` URIs from the local
    /// filesystem.
    ///
    /// Accepts only `file:` URIs; every other scheme is
    /// [`WorkspaceError::UnsupportedScheme`]. File contents that are not
    /// valid UTF-8 are [`WorkspaceError::InvalidEncoding`], other I/O
    /// failures keep their source error, and a read larger than
    /// [`Self::DEFAULT_MAX_BYTES`] (16 MiB) is [`WorkspaceError::TooLarge`],
    /// unless a builder configured a different limit. The provider neither
    /// restricts reads to the workspace roots nor caches: every lookup reads
    /// the filesystem again.
    #[derive(Debug, Clone)]
    pub struct OsFileProvider {
        max_bytes: u64,
    }

    impl OsFileProvider {
        /// The default maximum size, in bytes, of one read: 16 MiB.
        pub const DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;

        /// An `OsFileProvider` with the default 16 MiB byte limit.
        pub fn new() -> Self {
            Self::default()
        }

        /// Begin configuring an `OsFileProvider` with a custom byte limit.
        pub fn builder() -> OsFileProviderBuilder {
            OsFileProviderBuilder::default()
        }
    }

    impl Default for OsFileProvider {
        fn default() -> Self {
            Self {
                max_bytes: Self::DEFAULT_MAX_BYTES,
            }
        }
    }

    /// Configures an [`OsFileProvider`] before it is built.
    #[derive(Debug, Clone)]
    pub struct OsFileProviderBuilder {
        max_bytes: u64,
    }

    impl Default for OsFileProviderBuilder {
        fn default() -> Self {
            Self {
                max_bytes: OsFileProvider::DEFAULT_MAX_BYTES,
            }
        }
    }

    impl OsFileProviderBuilder {
        /// Set the maximum number of bytes one read may return; a larger
        /// resource fails with [`WorkspaceError::TooLarge`].
        pub fn max_bytes(mut self, max_bytes: u64) -> Self {
            self.max_bytes = max_bytes;
            self
        }

        /// Build the configured `OsFileProvider`.
        pub fn build(self) -> OsFileProvider {
            OsFileProvider {
                max_bytes: self.max_bytes,
            }
        }
    }

    impl FileProvider for OsFileProvider {
        async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
            let scheme = uri
                .scheme()
                .map(|scheme| scheme.as_str())
                .unwrap_or_default();
            if !scheme.eq_ignore_ascii_case("file") {
                return Err(WorkspaceError::UnsupportedScheme(scheme.to_string()));
            }
            let Some(path) = native_path(uri)? else {
                return Ok(None);
            };
            let file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(WorkspaceError::Io(error)),
            };
            let mut bytes = Vec::new();
            file.take(self.max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)
                .await
                .map_err(WorkspaceError::Io)?;
            if bytes.len() as u64 > self.max_bytes {
                return Err(WorkspaceError::TooLarge {
                    limit: self.max_bytes,
                });
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| WorkspaceError::InvalidEncoding)
        }
    }

    /// The native path a `file:` URI names, or `Ok(None)` when this platform
    /// has no such file — a non-local authority on POSIX, a missing drive
    /// letter on Windows. Percent-decoding is strict: a path that decodes to
    /// non-UTF-8 bytes is [`WorkspaceError::InvalidEncoding`].
    fn native_path(uri: &Uri) -> Result<Option<PathBuf>, WorkspaceError> {
        #[cfg(windows)]
        {
            windows_native_path(uri)
        }
        #[cfg(not(windows))]
        {
            posix_native_path(uri)
        }
    }

    #[cfg(windows)]
    fn windows_native_path(uri: &Uri) -> Result<Option<PathBuf>, WorkspaceError> {
        let host = uri
            .authority()
            .map(|authority| authority.host().as_str())
            .unwrap_or_default();
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            let mut out = format!(r"\\{host}");
            for segment in uri
                .path()
                .as_str()
                .split('/')
                .filter(|segment| !segment.is_empty())
            {
                let decoded =
                    percent_decode_strict(segment).ok_or(WorkspaceError::InvalidEncoding)?;
                out.push('\\');
                out.push_str(&decoded);
            }
            return Ok(Some(PathBuf::from(out)));
        }
        let Some(rest) = uri.path().as_str().strip_prefix('/') else {
            return Ok(None);
        };
        let (drive, remainder) = rest.split_once('/').unwrap_or((rest, ""));
        let drive = percent_decode_strict(drive).ok_or(WorkspaceError::InvalidEncoding)?;
        if !is_windows_drive(&drive) {
            return Ok(None);
        }
        let mut segments = remainder.split('/').filter(|segment| !segment.is_empty());
        let mut out = drive;
        let Some(first) = segments.next() else {
            out.push('\\');
            return Ok(Some(PathBuf::from(out)));
        };
        for segment in std::iter::once(first).chain(segments) {
            let decoded = percent_decode_strict(segment).ok_or(WorkspaceError::InvalidEncoding)?;
            out.push('\\');
            out.push_str(&decoded);
        }
        Ok(Some(PathBuf::from(out)))
    }

    #[cfg(windows)]
    fn is_windows_drive(decoded: &str) -> bool {
        decoded.len() == 2
            && decoded.as_bytes()[0].is_ascii_alphabetic()
            && decoded.as_bytes()[1] == b':'
    }

    #[cfg(not(windows))]
    fn posix_native_path(uri: &Uri) -> Result<Option<PathBuf>, WorkspaceError> {
        let host = uri
            .authority()
            .map(|authority| authority.host().as_str())
            .unwrap_or_default();
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Ok(None);
        }
        let decoded =
            percent_decode_strict(uri.path().as_str()).ok_or(WorkspaceError::InvalidEncoding)?;
        if !decoded.starts_with('/') {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(decoded)))
    }

    #[cfg(test)]
    mod tests {
        use std::path::Path;
        use std::str::FromStr;

        use super::*;
        use crate::test_util::file_uri;

        fn uri(spelling: &str) -> Uri {
            Uri::from_str(spelling).expect("the test URI parses")
        }

        fn write(path: &Path, bytes: &[u8]) {
            std::fs::write(path, bytes).expect("the test file writes");
        }

        #[test]
        fn percent_encoded_paths_decode_to_native_paths() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("space name.txt");
            write(&file, b"text");

            let path = native_path(&file_uri(&file)).expect("the URI converts");
            assert_eq!(path, Some(std::path::absolute(&file).unwrap()));
        }

        #[test]
        fn non_utf8_percent_encoded_paths_are_invalid_encoding() {
            assert!(matches!(
                native_path(&uri("file:///a%FFb.txt")),
                Err(WorkspaceError::InvalidEncoding)
            ));
        }

        #[test]
        fn a_file_uri_without_a_rooted_path_names_no_file() {
            assert_eq!(native_path(&uri("file:relative.txt")).unwrap(), None);
        }

        #[cfg(windows)]
        #[test]
        fn drive_letter_spellings_name_one_windows_file() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("x.txt");
            write(&file, b"drive");
            let text = std::path::absolute(&file).unwrap();
            let text = text.to_str().unwrap().replace('\\', "/");
            let (letter, tail) = text.split_at(1);
            assert_eq!(tail.as_bytes()[0], b':');
            let rest = &tail[1..];
            for spelling in [
                format!("file:///{letter}{tail}"),
                format!("file:///{}%3A{rest}", letter.to_lowercase()),
                format!("file:///{}%3a{rest}", letter.to_lowercase()),
            ] {
                assert_eq!(
                    native_path(&uri(&spelling)).unwrap(),
                    Some(std::path::absolute(&file).unwrap()),
                    "{spelling}"
                );
            }
        }

        #[cfg(windows)]
        #[test]
        fn unc_file_uris_become_unc_paths() {
            assert_eq!(
                native_path(&uri("file://server/share/dir/file.txt")).unwrap(),
                Some(PathBuf::from(r"\\server\share\dir\file.txt")),
            );
            assert_eq!(
                native_path(&uri("file://server/share")).unwrap(),
                Some(PathBuf::from(r"\\server\share")),
            );
        }

        #[cfg(windows)]
        #[test]
        fn a_driveless_windows_file_uri_names_no_file() {
            assert_eq!(native_path(&uri("file:///Users/me/x.txt")).unwrap(), None);
        }

        #[cfg(windows)]
        #[test]
        fn a_localhost_authority_is_not_a_unc_host() {
            assert_eq!(
                native_path(&uri("file://localhost/C:/x.txt")).unwrap(),
                Some(PathBuf::from(r"C:\x.txt")),
            );
        }

        #[cfg(not(windows))]
        #[test]
        fn posix_hosts_and_localhost_decode_to_posix_paths() {
            assert_eq!(
                native_path(&uri("file://server/share/x.txt")).unwrap(),
                None
            );
            assert_eq!(
                native_path(&uri("file://localhost/tmp/x.txt")).unwrap(),
                Some(PathBuf::from("/tmp/x.txt")),
            );
        }

        #[tokio::test]
        async fn reads_percent_encoded_file_uris() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("space name.txt");
            write(&file, b"percent");
            let provider = OsFileProvider::new();

            assert_eq!(
                provider
                    .read_text(&file_uri(&file))
                    .await
                    .unwrap()
                    .as_deref(),
                Some("percent")
            );
        }

        #[tokio::test]
        async fn accepts_file_schemes_case_insensitively() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("x.txt");
            write(&file, b"case");
            let spelling = file_uri(&file).as_str().replacen("file://", "FILE://", 1);
            let provider = OsFileProvider::new();

            assert_eq!(
                provider
                    .read_text(&uri(&spelling))
                    .await
                    .unwrap()
                    .as_deref(),
                Some("case")
            );
        }

        #[tokio::test]
        async fn rejects_every_non_file_scheme() {
            let provider = OsFileProvider::new();
            for spelling in [
                "untitled:Untitled-1",
                "untitled:///x.rs",
                "http://example.com/a.rs",
                "/no/scheme",
            ] {
                let parsed = uri(spelling);
                let expected_scheme = parsed.scheme().map(|s| s.as_str()).unwrap_or_default();
                let err = provider.read_text(&parsed).await.unwrap_err();
                assert!(
                    matches!(err, WorkspaceError::UnsupportedScheme(ref scheme) if scheme == expected_scheme),
                    "{spelling} should be unsupported, got {err:?}"
                );
            }
        }

        #[tokio::test]
        async fn a_missing_file_is_none() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let provider = OsFileProvider::new();

            assert_eq!(
                provider
                    .read_text(&file_uri(&dir.path().join("absent.txt")))
                    .await
                    .unwrap(),
                None
            );
        }

        #[tokio::test]
        async fn non_utf8_contents_are_invalid_encoding() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("blob.bin");
            write(&file, &[0xFF, 0xFE, 0x00]);
            let provider = OsFileProvider::new();

            assert!(matches!(
                provider.read_text(&file_uri(&file)).await,
                Err(WorkspaceError::InvalidEncoding)
            ));
        }

        #[tokio::test]
        async fn default_limit_accepts_16_mib_and_rejects_more() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let small = dir.path().join("small.bin");
            write(
                &small,
                &vec![b'x'; OsFileProvider::DEFAULT_MAX_BYTES as usize],
            );
            let large = dir.path().join("large.bin");
            write(
                &large,
                &vec![b'x'; OsFileProvider::DEFAULT_MAX_BYTES as usize + 1],
            );
            let provider = OsFileProvider::new();

            assert!(
                provider.read_text(&file_uri(&small)).await.is_ok(),
                "a read at exactly the limit succeeds"
            );
            assert!(matches!(
                provider.read_text(&file_uri(&large)).await,
                Err(WorkspaceError::TooLarge { limit }) if limit == OsFileProvider::DEFAULT_MAX_BYTES
            ));
        }

        #[tokio::test]
        async fn builder_sets_a_custom_byte_limit() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let ok = dir.path().join("ok.txt");
            write(&ok, &[b'x'; 16]);
            let over = dir.path().join("over.txt");
            write(&over, &[b'x'; 17]);
            let provider = OsFileProvider::builder().max_bytes(16).build();

            assert_eq!(
                provider
                    .read_text(&file_uri(&ok))
                    .await
                    .unwrap()
                    .unwrap()
                    .len(),
                16,
                "a read at exactly the configured limit succeeds"
            );
            assert!(matches!(
                provider.read_text(&file_uri(&over)).await,
                Err(WorkspaceError::TooLarge { limit: 16 })
            ));
        }

        #[tokio::test]
        async fn io_failures_keep_their_source_error() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let provider = OsFileProvider::new();
            #[cfg(windows)]
            let expected_kind = std::io::ErrorKind::PermissionDenied;
            #[cfg(not(windows))]
            let expected_kind = std::io::ErrorKind::IsADirectory;

            let err = provider
                .read_text(&file_uri(dir.path()))
                .await
                .expect_err("a directory is not a readable text file");
            assert!(
                matches!(err, WorkspaceError::Io(ref source) if source.kind() == expected_kind),
                "the os-level failure is kept, got {err:?}"
            );
        }

        #[tokio::test]
        async fn reads_are_not_cached() {
            let dir = tempfile::tempdir().expect("a tempdir is created");
            let file = dir.path().join("live.txt");
            write(&file, b"one");
            let provider = OsFileProvider::new();
            let uri = file_uri(&file);

            assert_eq!(
                provider.read_text(&uri).await.unwrap().as_deref(),
                Some("one")
            );
            write(&file, b"two");
            assert_eq!(
                provider.read_text(&uri).await.unwrap().as_deref(),
                Some("two"),
                "the second read sees the rewritten file"
            );
        }

        #[tokio::test]
        async fn a_file_uri_this_platform_cannot_address_is_none() {
            let provider = OsFileProvider::new();
            #[cfg(windows)]
            let spelling = "file:///Users/me/x.txt";
            #[cfg(not(windows))]
            let spelling = "file://server/share/x.txt";

            assert_eq!(provider.read_text(&uri(spelling)).await.unwrap(), None);
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
pub use os::{OsFileProvider, OsFileProviderBuilder};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::str::FromStr;

    use lsp_types::Uri;

    use super::EmptyFileProvider;
    use crate::file_provider::FileProvider;
    use crate::workspace::WorkspaceError;

    #[tokio::test]
    async fn the_empty_provider_reports_every_scheme_as_unsupported() {
        let provider = EmptyFileProvider;
        for spelling in [
            "file:///tmp/x.txt",
            "untitled:Untitled-1",
            "http://example.com/a.rs",
            "/no/scheme",
        ] {
            let uri = Uri::from_str(spelling).expect("the test URI parses");
            let expected_scheme = uri.scheme().map(|s| s.as_str()).unwrap_or_default();
            let error = provider.read_text(&uri).await.unwrap_err();
            assert!(
                matches!(error, WorkspaceError::UnsupportedScheme(ref scheme) if scheme == expected_scheme),
                "{spelling} should be unsupported, got {error:?}"
            );
        }
    }
}
