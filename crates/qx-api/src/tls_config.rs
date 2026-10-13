use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::path::Path;
use std::sync::Arc;

/// 构造要求客户端证书的 TLS 服务端配置；证书解析、私钥加载和信任根管理由部署边界负责。
pub fn build_mtls_server_config(
    client_roots: RootCertStore,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, String> {
    let verifier = WebPkiClientVerifier::builder(Arc::new(client_roots))
        .build()
        .map_err(|error| format!("mTLS 客户端证书校验器构造失败: {error}"))?;
    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificate_chain, private_key)
        .map_err(|error| format!("mTLS 服务端证书配置非法: {error}"))?;
    Ok(Arc::new(config))
}

/// 从 PEM 文件加载 mTLS 服务端配置。解析失败会在替换配置前返回。
pub fn load_mtls_server_config_from_pem(
    certificate_chain_path: impl AsRef<Path>,
    private_key_path: impl AsRef<Path>,
    client_ca_path: impl AsRef<Path>,
) -> Result<Arc<ServerConfig>, String> {
    let certificate_chain = CertificateDer::pem_file_iter(certificate_chain_path.as_ref())
        .map_err(|error| format!("读取 mTLS 服务端证书链失败: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 mTLS 服务端证书链失败: {error}"))?;
    if certificate_chain.is_empty() {
        return Err("mTLS 服务端证书链不能为空".into());
    }

    let private_key = PrivateKeyDer::from_pem_file(private_key_path.as_ref())
        .map_err(|error| format!("解析 mTLS 服务端私钥失败: {error}"))?;
    let mut client_roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_file_iter(client_ca_path.as_ref())
        .map_err(|error| format!("读取 mTLS 客户端 CA 失败: {error}"))?
    {
        let certificate =
            certificate.map_err(|error| format!("解析 mTLS 客户端 CA 失败: {error}"))?;
        client_roots
            .add(certificate)
            .map_err(|error| format!("加入 mTLS 客户端 CA 失败: {error}"))?;
    }
    if client_roots.is_empty() {
        return Err("mTLS 客户端 CA 不能为空".into());
    }
    build_mtls_server_config(client_roots, certificate_chain, private_key)
}

/// 加载一个 PEM 文件中的证书链；用于构造 mTLS 证书到 Operator 的显式映射。
pub fn load_certificate_chain_from_pem(
    certificate_chain_path: impl AsRef<Path>,
) -> Result<Vec<CertificateDer<'static>>, String> {
    let certificates = CertificateDer::pem_file_iter(certificate_chain_path.as_ref())
        .map_err(|error| format!("读取 PEM 证书链失败: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 PEM 证书链失败: {error}"))?;
    if certificates.is_empty() {
        return Err("PEM 证书链不能为空".into());
    }
    Ok(certificates)
}
