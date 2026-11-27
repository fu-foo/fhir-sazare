//! TLS support for the FHIR server
//!
//! Implements `axum::serve::Listener` for TLS-wrapped TCP connections.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Longest a client may take to complete the TLS handshake.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Handshakes in progress at once; further connections wait to be accepted.
const MAX_HANDSHAKES: usize = 1024;

/// A TLS-wrapped TCP listener that implements `axum::serve::Listener`.
///
/// Handshakes run concurrently in their own tasks, each with a timeout: a
/// client that opens a connection and never finishes the handshake (or does
/// so slowly) can't hold up anyone else's connection.
pub struct TlsListener {
    ready: tokio::sync::mpsc::Receiver<(tokio_rustls::server::TlsStream<tokio::net::TcpStream>, SocketAddr)>,
    local_addr: io::Result<SocketAddr>,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> Self {
        let local_addr = tcp.local_addr();
        let (tx, ready) = tokio::sync::mpsc::channel(MAX_HANDSHAKES);
        let slots = Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES));
        tokio::spawn(async move {
            loop {
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    return;
                };
                // Stop (and release the port) as soon as the listener is
                // dropped, not only after the next connection arrives.
                let accepted = tokio::select! {
                    _ = tx.closed() => return,
                    accepted = tcp.accept() => accepted,
                };
                let (stream, addr) = match accepted {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!("TCP accept error: {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let conn_tx = tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = conn_tx.send((tls, addr)).await;
                        }
                        Ok(Err(e)) => tracing::warn!("TLS handshake failed from {}: {}", addr, e),
                        Err(_) => tracing::warn!("TLS handshake from {} timed out", addr),
                    }
                });
            }
        });
        Self { ready, local_addr }
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(conn) => conn,
            // The accept task ends only once this listener is dropped.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        match &self.local_addr {
            Ok(a) => Ok(*a),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }
}

/// Connect-info type for TLS connections.
///
/// axum's built-in `Connected` impl for `SocketAddr` only covers plain
/// `TcpListener`, and orphan rules forbid adding one for `SocketAddr` + our
/// `TlsListener`. So we use this local newtype as the connect-info type and
/// remap it to `ConnectInfo<SocketAddr>` (what handlers expect) via
/// [`propagate_connect_info`]. Without this, handlers that extract
/// `ConnectInfo<SocketAddr>` (bundle, bulk) return 500 over HTTPS because the
/// connect-info extension is never inserted.
#[derive(Clone, Copy, Debug)]
pub struct TlsConnectInfo(pub SocketAddr);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for TlsConnectInfo
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        TlsConnectInfo(*stream.remote_addr())
    }
}

/// Middleware that copies the TLS `ConnectInfo<TlsConnectInfo>` extension into
/// the `ConnectInfo<SocketAddr>` extension that shared handlers extract, so the
/// same handlers work identically over HTTP and HTTPS.
pub async fn propagate_connect_info(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::extract::connect_info::ConnectInfo;
    if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none()
        && let Some(&ConnectInfo(TlsConnectInfo(addr))) =
            req.extensions().get::<ConnectInfo<TlsConnectInfo>>()
    {
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    next.run(req).await
}

/// Under the built-in TLS, a request without `X-Forwarded-Proto` came over
/// https: say so, so every URL built from the request (links, Location,
/// the CapabilityStatement) is https.
pub async fn mark_https(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if !req.headers().contains_key("x-forwarded-proto") {
        req.headers_mut().insert("x-forwarded-proto", axum::http::HeaderValue::from_static("https"));
    }
    next.run(req).await
}

/// Load TLS certificate and private key, returning a `TlsAcceptor`.
pub fn load_tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<TlsAcceptor, Box<dyn std::error::Error>> {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| format!("Failed to open cert file '{}': {}", cert_path, e))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("Failed to parse certificates: {}", e))?;

    if certs.is_empty() {
        return Err("No certificates found in cert file".into());
    }

    let key = PrivateKeyDer::from_pem_file(key_path)
        .map_err(|e| format!("Failed to read a private key from '{}': {}", key_path, e))?;

    // Explicitly select ring as crypto provider (both ring and aws-lc-rs may be
    // in the dependency tree via reqwest, preventing auto-detection)
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS protocol error: {}", e))?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|e| format!("Invalid TLS configuration: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}
