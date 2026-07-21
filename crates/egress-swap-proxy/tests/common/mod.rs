//! Shared test helpers for egress-swap-proxy integration tests.
//!
//! cr-12 G2 review I-5:rcgen-based ephemeral cert generation 搬到此处,
//! 让 rcgen 保持 dev-dependency(不进 production binary)。Task 4 的 E2E
//! (`tests/swap_e2e.rs`)会用 `mod common; use common::ephemeral_self_signed;`
//! 生成证 → 经 `FIXUS_SWAP_CERT_PEM` / `FIXUS_SWAP_KEY_PEM` env 注入 swap-proxy。

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// 生成一份 ephemeral 自签 cert + key(CN=localhost),供 swap-proxy 入站 TLS 测试用。
///
/// 返回 `(cert_chain, key_der, cert_pem)`:
/// - `cert_chain` / `key_der` —— 可直接喂 `rustls::ServerConfig::with_single_cert`(fake 上游用)。
/// - `cert_pem` —— PEM 文本,设进 `FIXUS_SWAP_CERT_PEM` env 喂 swap-proxy;同时可拆出
///   给测试 client 作信任根(swap-proxy 的入站证 = 此 cert)。
#[allow(dead_code)] // Task 4 E2E 将引用
pub fn ephemeral_self_signed() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>, String) {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("rcgen self-signed cert");
    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.signing_key.serialize_pem();
    let chain = parse_chain(&cert_pem);
    let key = parse_key(&key_pem);
    (chain, key, cert_pem)
}

/// 同 [`ephemeral_self_signed`],但额外返回 key_pem(swap-proxy 需经 env 收 cert+key 两者)。
#[allow(dead_code)] // Task 4 E2E 将引用
pub fn ephemeral_self_signed_with_key_pem() -> (
    Vec<CertificateDer<'static>>,
    PrivateKeyDer<'static>,
    String,
    String,
) {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("rcgen self-signed cert");
    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.signing_key.serialize_pem();
    let chain = parse_chain(&cert_pem);
    let key = parse_key(&key_pem);
    (chain, key, cert_pem, key_pem)
}

/// 同 [`ephemeral_self_signed_with_key_pem`],但 SAN 由调用方指定。
///
/// rcgen 0.14 的 `CertificateParams::new` 自动识别 IP 字面量:
/// `"127.0.0.1"` → `SanType::IpAddress`、`"localhost"` → `SanType::DnsName`。
///
/// Task 4 E2E 用 `&["127.0.0.1"]` 生成 fake upstream 证 —— swap-proxy 经
/// `ServerName::IpAddress(127.0.0.1)` 连入时,只有 IP SAN 能通过 rustls 验证。
#[allow(dead_code)]
pub fn ephemeral_self_signed_for(
    sans: &[&str],
) -> (
    Vec<CertificateDer<'static>>,
    PrivateKeyDer<'static>,
    String,
    String,
) {
    let sans_owned: Vec<String> = sans.iter().map(|s| s.to_string()).collect();
    let certified_key = rcgen::generate_simple_self_signed(sans_owned)
        .expect("rcgen self-signed cert with custom SANs");
    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.signing_key.serialize_pem();
    let chain = parse_chain(&cert_pem);
    let key = parse_key(&key_pem);
    (chain, key, cert_pem, key_pem)
}

fn parse_chain(pem: &str) -> Vec<CertificateDer<'static>> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("valid cert pem")
}

fn parse_key(pem: &str) -> PrivateKeyDer<'static> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .expect("valid key pem")
        .expect("at least one private key")
}
