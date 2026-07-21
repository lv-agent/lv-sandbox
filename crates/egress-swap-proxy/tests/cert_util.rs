//! cr-12 G2 review I-5:冒烟测试 —— 验证搬出 main.rs 后的 rcgen 自签助手仍能跑通,
//! 且产物可被 rustls 接受为 ServerConfig(这是 main.rs 在运行期做的事)。
//! 同时把 rcgen 锁定为 dev-dep(production binary 不含此路径)。

mod common;

use common::ephemeral_self_signed;

#[test]
fn ephemeral_cert_yields_usable_server_config() {
    let (chain, key, cert_pem) = ephemeral_self_signed();

    // 基本形状
    assert!(!chain.is_empty(), "cert chain must be non-empty");
    assert!(
        cert_pem.contains("BEGIN CERTIFICATE"),
        "cert PEM looks like a cert: {cert_pem}"
    );

    // 真验证:rustls 接受这份 cert+key(= main.rs::build_server_config 的运行期路径)。
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key);
    assert!(cfg.is_ok(), "rustls must accept ephemeral cert + key");
}
