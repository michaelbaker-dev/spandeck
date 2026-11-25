//! The one place an outgoing HTTPS client is made (the vault, releases,
//! the price list, other stations), so they all trust the same things.
//!
//! Certificates are checked by the system's own verifier everywhere
//! (reqwest's default: the platform's trust store and its rules). On Windows
//! that store is filled on demand: a root Windows has never needed is not on
//! the machine until something makes Windows fetch it. On the owner's new
//! Windows machine the first contact with the vault failed with
//! `UnknownIssuer` although the vault's certificate was good, and went on
//! failing until another program (curl, through schannel) had connected to
//! the same address once. So on Windows the Mozilla root list that ships
//! inside spandeck (`webpki-root-certs`) is a second source of roots: when
//! Windows cannot build a chain to a root it has, the same Windows verifier
//! is asked again with those roots. Nothing is checked less: names, dates,
//! usage and signatures are verified by Windows either way, and a
//! certificate that chains to no public root still fails.
//! (DECISIONS.md, "Windows".)

use spandeck_terminal::sys;

/// A client builder with spandeck's trust settings. Use this, never
/// `reqwest::Client::builder()` directly.
pub fn builder() -> reqwest::ClientBuilder {
    // A connection to another station that went quiet is asked after: one
    // whose other end is gone (this machine slept, or changed networks)
    // is closed, and not kept and waited on.
    let builder = reqwest::Client::builder().tcp_keepalive(std::time::Duration::from_secs(30));
    if !sys::WINDOWS {
        return builder;
    }
    builder.tls_certs_merge(public_roots())
}

/// The public roots carried in the program, as reqwest wants them.
fn public_roots() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der.as_ref()).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    /// The reason: the Windows fix depends on the carried root list being
    /// usable, and on it holding the roots the vault's certificate (Let's
    /// Encrypt, through Tailscale) chains to. Both are checked on every
    /// system, since the list is the same file everywhere.
    #[test]
    fn the_carried_roots_load_and_include_lets_encrypts() {
        let roots = super::public_roots();
        assert_eq!(
            roots.len(),
            webpki_root_certs::TLS_SERVER_ROOT_CERTS.len(),
            "every carried root is one reqwest accepts"
        );
        assert!(roots.len() > 100);
        // "ISRG Root X1" and "ISRG Root X2" appear in their certificates as text.
        for name in ["ISRG Root X1", "ISRG Root X2"] {
            assert!(
                webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().any(|der| {
                    der.as_ref()
                        .windows(name.len())
                        .any(|w| w == name.as_bytes())
                }),
                "{name} is carried"
            );
        }
        // A client with them merged in can be built (on Windows this is the
        // client spandeck uses; elsewhere it shows the option is accepted).
        reqwest::Client::builder()
            .tls_certs_merge(roots)
            .build()
            .unwrap();
    }

    /// With the carried roots merged in, a good public certificate is still
    /// accepted and a certificate no public root stands behind is still
    /// refused. Run by hand on a Windows machine (`-- --ignored`): it needs
    /// the internet, and it is on Windows that the merged roots are used.
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn a_good_certificate_passes_and_an_untrusted_one_is_still_refused() {
        let http = super::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();
        http.get("https://github.com/").send().await.unwrap();
        for bad in [
            "https://untrusted-root.badssl.com/",
            "https://self-signed.badssl.com/",
            "https://expired.badssl.com/",
            "https://wrong.host.badssl.com/",
        ] {
            let err = http.get(bad).send().await.expect_err(bad);
            assert!(err.is_connect(), "{bad}: {err:?}");
        }
    }
}
