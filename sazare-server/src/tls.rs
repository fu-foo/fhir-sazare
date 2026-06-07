//! TLS support for the FHIR server
//!
//! Implements `axum::serve::Listener` for TLS-wrapped TCP connections.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// A TLS-wrapped TCP listener that implements `axum::serve::Listener`.
pub struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> Self {
        Self { tcp, acceptor }
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = match self.tcp.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::error!("TCP accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };

            match self.acceptor.accept(stream).await {
                Ok(tls_stream) => return (tls_stream, addr),
                Err(e) => {
                    tracing::warn!("TLS handshake failed from {}: {}", addr, e);
                    continue;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
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

/// Load TLS certificate and private key, returning a `TlsAcceptor`.
pub fn load_tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<TlsAcceptor, Box<dyn std::error::Error>> {
    use std::io::BufReader;

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| format!("Failed to open cert file '{}': {}", cert_path, e))?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| format!("Failed to open key file '{}': {}", key_path, e))?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("Failed to parse certificates: {}", e))?;

    if certs.is_empty() {
        return Err("No certificates found in cert file".into());
    }

    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| format!("Failed to parse private key: {}", e))?
        .ok_or("No private key found in key file")?;

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
