//! TLS on the station's own name, ended here (DECISIONS.md, "HTTPS, served
//! by spandeckd itself"): the caller's real address is still what is
//! checked, which it would not be behind `tailscale serve`.
//!
//! The certificate is asked for at each handshake, so a renewed one is
//! used by the next connection and the open ones carry on.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use axum::extract::ConnectInfo;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

/// Put on every request that arrived over TLS.
#[derive(Clone, Copy)]
pub struct Secure;

/// The certificate in use. Replaced whole; never read by anything but a
/// handshake.
#[derive(Default)]
pub struct Certs(RwLock<Option<Arc<CertifiedKey>>>);

impl std::fmt::Debug for Certs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Certs")
    }
}

impl Certs {
    pub fn set(&self, to: Option<CertifiedKey>) {
        *self.0.write().unwrap() = to.map(Arc::new);
    }

    pub fn held(&self) -> bool {
        self.0.read().unwrap().is_some()
    }
}

impl ResolvesServerCert for Certs {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0.read().unwrap().clone()
    }
}

/// A chain and its key from PEM text, checked to belong together.
pub fn certified(cert_pem: &str, key_pem: &str) -> anyhow::Result<CertifiedKey> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("the certificate cannot be read: {e}"))?;
    anyhow::ensure!(!chain.is_empty(), "no certificate");
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("the certificate's key cannot be read: {e}"))?;
    let key = aws_lc_rs::default_provider()
        .key_provider
        .load_private_key(key)
        .map_err(|e| anyhow::anyhow!("the certificate's key cannot be used: {e}"))?;
    let certified = CertifiedKey::new(chain, key);
    certified
        .keys_match()
        .map_err(|e| anyhow::anyhow!("the key is not this certificate's: {e}"))?;
    Ok(certified)
}

pub fn acceptor(certs: Arc<Certs>) -> anyhow::Result<TlsAcceptor> {
    let mut config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(certs);
    // The deck's socket is an HTTP/1.1 upgrade.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Accept for ever. Each connection is served by a task of its own, so
/// that ending this (the address is gone, HTTPS was turned off) closes no
/// connection that is open.
pub async fn serve(
    listener: tokio::net::TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
    each: impl Fn(&tokio::net::TcpStream) + Send + 'static,
) {
    loop {
        let (tcp, addr): (_, SocketAddr) = match listener.accept().await {
            Ok(got) => got,
            Err(e) => {
                tracing::debug!(error = %e, "a TLS connection was not accepted");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        each(&tcp);
        let (acceptor, app) = (acceptor.clone(), app.clone());
        tokio::spawn(async move {
            // A caller that connects and says nothing holds nothing open.
            let hand =
                tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(tcp));
            let Ok(Ok(tls)) = hand.await else {
                return;
            };
            let service = hyper::service::service_fn(
                move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(ConnectInfo(addr));
                    req.extensions_mut().insert(Secure);
                    app.clone().oneshot(req)
                },
            );
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(tls), service)
                .await;
        });
    }
}
