//! Setting a connection's certificate, as Pingora's `tls::ext`

use openssl::{
    error::ErrorStack,
    pkey::{PKeyRef, Private},
    ssl::SslRef,
    x509::X509Ref,
};

/// # Errors
///
/// When OpenSSL refuses the key.
pub fn ssl_use_private_key(ssl: &mut SslRef, key: &PKeyRef<Private>) -> Result<(), ErrorStack> {
    ssl.set_private_key(key)
}

/// # Errors
///
/// When OpenSSL refuses the certificate.
pub fn ssl_use_certificate(ssl: &mut SslRef, cert: &X509Ref) -> Result<(), ErrorStack> {
    ssl.set_certificate(cert)
}

/// # Errors
///
/// When OpenSSL refuses the certificate.
pub fn ssl_add_chain_cert(ssl: &mut SslRef, cert: &X509Ref) -> Result<(), ErrorStack> {
    ssl.add_chain_cert(cert.to_owned())
}
