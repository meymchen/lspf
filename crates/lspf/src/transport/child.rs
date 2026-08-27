//! Launch and supervise one native stdio language-server child.

use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::client_endpoint::{ClientBuilder, ClientConnection as ProtocolConnection, ServerHandle};
use crate::transport::StdioTransport;
use crate::{BuildError, ClientError, Error as ConnectionError, Outcome};

const EXIT_GRACE: Duration = Duration::from_secs(1);
const TERMINATE_GRACE: Duration = Duration::from_secs(1);
const STDERR_CAPTURE_LIMIT: usize = 64 * 1024;

/// A failure while building, launching, driving, or reclaiming a stdio child.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChildError {
    /// Static Client configuration was invalid.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The language-server process could not be spawned or reclaimed.
    #[error("child process operation failed: {0}")]
    Process(#[source] io::Error),
    /// The Client connection failed during initialization or serving.
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    /// The graceful LSP shutdown or exit transition failed.
    #[error(transparent)]
    Lifecycle(#[from] ClientError),
    /// An internal child-supervision task panicked or was cancelled.
    #[error("child supervision task failed: {0}")]
    Supervision(#[from] tokio::task::JoinError),
    /// An internal child-supervision task did not stop within its cleanup bound.
    #[error("{0} did not stop within the child cleanup deadline")]
    SupervisionTimeout(&'static str),
}

/// The terminal result of a supervised stdio language-server child.
#[derive(Debug)]
pub struct ChildOutput {
    outcome: Outcome,
    status: ExitStatus,
    stderr: Vec<u8>,
    stderr_truncated: bool,
}

impl ChildOutput {
    /// How the LSP connection ended.
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// The operating-system exit status after the child was reaped.
    pub fn status(&self) -> ExitStatus {
        self.status
    }

    /// The first 64 KiB written by the child to stderr.
    ///
    /// The supervisor keeps draining after this limit so a noisy child cannot
    /// deadlock on a full stderr pipe.
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Whether stderr contained bytes beyond [`Self::stderr`].
    pub fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

struct StderrCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

/// An initialized Client connection that owns and supervises its stdio child.
///
/// The incoming protocol driver and stderr drain run as soon as this value is
/// returned. Call [`Self::server`] for typed traffic, [`Self::shutdown`] for a
/// complete graceful lifecycle, or [`Self::wait`] when the child is expected
/// to end by itself. Dropping the connection starts a reaper thread that uses
/// Tokio for graceful cleanup when available and always retains a synchronous
/// kill-and-reap fallback across runtime shutdown. Without a current runtime,
/// Drop performs that fallback synchronously.
pub struct ChildConnection {
    server: ServerHandle,
    supervision: Option<SupervisionState>,
}

struct SupervisionState {
    driver: JoinHandle<crate::Result<Outcome>>,
    child: Child,
    stderr: JoinHandle<io::Result<StderrCapture>>,
}

enum FinishMode {
    Cleanup,
    NaturalExit,
}

impl ClientBuilder {
    /// Spawn and initialize an arbitrary language server over piped stdio.
    ///
    /// The command's stdin, stdout, and stderr settings are replaced with
    /// pipes. Stderr is drained concurrently and captured up to a fixed bound;
    /// stdout remains exclusively owned by the LSP framing adapter.
    pub async fn spawn(self, mut command: Command) -> Result<ChildConnection, ChildError> {
        let builder = self.validate()?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(ChildError::Process)?;
        let stdin = child
            .stdin
            .take()
            .expect("piped child stdin is present after spawn");
        let stdout = child
            .stdout
            .take()
            .expect("piped child stdout is present after spawn");
        let stderr = child
            .stderr
            .take()
            .expect("piped child stderr is present after spawn");
        let stderr = tokio::spawn(drain_stderr(stderr));
        let transport = StdioTransport::from_io(stdout, stdin);
        let client = match builder.build(transport) {
            Ok(client) => client,
            Err(error) => {
                cleanup_failed_start(&mut child, stderr).await;
                return Err(error.into());
            }
        };
        let connection = match client.connect().await {
            Ok(connection) => connection,
            Err(error) => {
                cleanup_failed_start(&mut child, stderr).await;
                return Err(error.into());
            }
        };

        Ok(ChildConnection::new(connection, child, stderr))
    }
}

impl ChildConnection {
    fn new(
        connection: ProtocolConnection,
        child: Child,
        stderr: JoinHandle<io::Result<StderrCapture>>,
    ) -> Self {
        let server = connection.server();
        let driver = tokio::spawn(connection.serve());
        Self {
            server,
            supervision: Some(SupervisionState {
                driver,
                child,
                stderr,
            }),
        }
    }

    /// Clone the typed handle used to communicate with the child server.
    pub fn server(&self) -> ServerHandle {
        self.server.clone()
    }

    /// The operating-system process identifier while the child is running.
    pub fn id(&self) -> u32 {
        self.supervision
            .as_ref()
            .and_then(|supervision| supervision.child.id())
            .expect("a live ChildConnection owns its process")
    }

    /// Complete `shutdown` followed by `exit`, then reap the child.
    ///
    /// Cleanup still disconnects and reclaims the process if either lifecycle
    /// operation fails.
    pub async fn shutdown(mut self) -> Result<ChildOutput, ChildError> {
        let lifecycle = match self.server.shutdown().await {
            Ok(()) => self.server.exit(),
            Err(error) => Err(error),
        };
        if lifecycle.is_err() {
            self.server.disconnect();
        }
        let result = self.finish(FinishMode::Cleanup).await;
        lifecycle?;
        result
    }

    /// Wait for a child that is expected to terminate without local cleanup.
    ///
    /// The process is still reaped and the protocol driver and stderr drain
    /// are joined before this future resolves.
    pub async fn wait(mut self) -> Result<ChildOutput, ChildError> {
        self.finish(FinishMode::NaturalExit).await
    }

    async fn finish(&mut self, mode: FinishMode) -> Result<ChildOutput, ChildError> {
        let supervision = self
            .supervision
            .as_mut()
            .expect("child supervision is owned once");
        let (driver_result, status_result) = match mode {
            FinishMode::Cleanup => (
                join_driver(&mut supervision.driver).await,
                reclaim(&mut supervision.child).await,
            ),
            FinishMode::NaturalExit => {
                let status = supervision.child.wait().await.map_err(ChildError::Process);
                (join_driver(&mut supervision.driver).await, status)
            }
        };
        let stderr_result = (&mut supervision.stderr)
            .await?
            .map_err(ChildError::Process);
        self.supervision.take();
        let outcome = driver_result?;
        let status = status_result?;
        let stderr = stderr_result?;
        Ok(ChildOutput {
            outcome,
            status,
            stderr: stderr.bytes,
            stderr_truncated: stderr.truncated,
        })
    }
}

impl Drop for ChildConnection {
    fn drop(&mut self) {
        let Some(mut supervision) = self.supervision.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let server = self.server.clone();
            let graceful = runtime.spawn(async move {
                let shutdown = tokio::time::timeout(EXIT_GRACE, server.shutdown()).await;
                if matches!(shutdown, Ok(Ok(()))) {
                    let _ = server.exit();
                } else {
                    server.disconnect();
                }
            });
            std::thread::spawn(move || {
                reclaim_without_runtime(&mut supervision.child);
                wait_for_cleanup_tasks(
                    &graceful,
                    &supervision.driver,
                    &supervision.stderr,
                    EXIT_GRACE,
                );
                graceful.abort();
                supervision.driver.abort();
                supervision.stderr.abort();
            });
        } else {
            self.server.disconnect();
            supervision.driver.abort();
            supervision.stderr.abort();
            reclaim_without_runtime(&mut supervision.child);
        }
    }
}

async fn cleanup_failed_start(child: &mut Child, stderr: JoinHandle<io::Result<StderrCapture>>) {
    let _ = child.kill().await;
    let _ = stderr.await;
}

async fn drain_stderr(mut stderr: impl tokio::io::AsyncRead + Unpin) -> io::Result<StderrCapture> {
    let mut bytes = Vec::with_capacity(STDERR_CAPTURE_LIMIT);
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = stderr.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = STDERR_CAPTURE_LIMIT.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok(StderrCapture { bytes, truncated })
}

async fn reclaim(child: &mut Child) -> Result<ExitStatus, ChildError> {
    if let Ok(Some(status)) = bounded_wait(child, EXIT_GRACE).await {
        return Ok(status);
    }
    let _ = terminate(child);
    if let Ok(Some(status)) = bounded_wait(child, TERMINATE_GRACE).await {
        return Ok(status);
    }
    match child.kill().await {
        Ok(()) => child.wait().await.map_err(ChildError::Process),
        Err(error) => match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            _ => Err(ChildError::Process(error)),
        },
    }
}

fn reclaim_without_runtime(child: &mut Child) {
    if bounded_wait_without_runtime(child, EXIT_GRACE) {
        return;
    }
    let _ = terminate(child);
    if bounded_wait_without_runtime(child, TERMINATE_GRACE) {
        return;
    }
    kill_and_reap_without_runtime(child);
}

fn bounded_wait_without_runtime(child: &mut Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

fn kill_and_reap_without_runtime(child: &mut Child) {
    loop {
        let _ = child.start_kill();
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

fn wait_for_cleanup_tasks<T, U>(
    graceful: &JoinHandle<()>,
    driver: &JoinHandle<T>,
    stderr: &JoinHandle<U>,
    timeout: Duration,
) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline
        && (!graceful.is_finished() || !driver.is_finished() || !stderr.is_finished())
    {
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn bounded_wait(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep((deadline - now).min(Duration::from_millis(10))).await;
    }
}

async fn join_driver(
    driver: &mut JoinHandle<crate::Result<Outcome>>,
) -> Result<Outcome, ChildError> {
    match tokio::time::timeout(EXIT_GRACE, &mut *driver).await {
        Ok(result) => Ok(result??),
        Err(_) => {
            driver.abort();
            let _ = driver.await;
            Err(ChildError::SupervisionTimeout("protocol driver"))
        }
    }
}

#[cfg(unix)]
fn terminate(child: &mut Child) -> io::Result<()> {
    use std::os::raw::c_int;

    unsafe extern "C" {
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }
    const SIGTERM: c_int = 15;

    let Some(pid) = child.id() else {
        return Ok(());
    };
    // SAFETY: POSIX `kill` takes the live child PID returned by Tokio and the
    // constant SIGTERM value; neither argument points to memory.
    let signalled = unsafe { kill(pid as c_int, SIGTERM) } == 0;
    if signalled || child.try_wait()?.is_some() {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn terminate(child: &mut Child) -> io::Result<()> {
    child.start_kill()
}
