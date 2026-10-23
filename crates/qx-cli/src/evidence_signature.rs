use crate::evidence_signature_args::{EvidenceArgs, EvidenceCommand};
use qx_spec::{
    CapabilityLevel, EvidenceBundle, EvidenceGeneration, EvidenceWindow, FoundationDocument,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

const SIGNATURE_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureEnvelope {
    schema_version: u32,
    algorithm: String,
    signature_hex: String,
}

#[derive(Serialize)]
struct SignatureResult<'a> {
    ok: bool,
    operation: &'a str,
    signature_algorithm: &'a str,
    generation: &'a str,
    capability_level: &'a str,
    venue_id: &'a str,
    window: &'a EvidenceWindow,
    signature_file: Option<String>,
    public_key_hex: Option<String>,
}

struct VenueIdentity<'a> {
    level: CapabilityLevel,
    venue_id: &'a str,
    window: &'a EvidenceWindow,
}

fn read_bundle(path: &Path) -> Result<EvidenceBundle, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 EvidenceBundle 失败 {}: {error}", path.display()))?;
    EvidenceBundle::from_json(&payload)
}

fn venue_identity(bundle: &EvidenceBundle) -> Result<VenueIdentity<'_>, String> {
    let identity = match &bundle.generation {
        EvidenceGeneration::Venue(venue) => VenueIdentity {
            level: venue.claim.capability_level,
            venue_id: &venue.venue_id,
            window: &venue.window,
        },
        EvidenceGeneration::Run(_) => {
            return Err("Ed25519 venue 验签只接受 generation=venue 的 EvidenceBundle".into());
        }
    };
    if identity.level < CapabilityLevel::L3 {
        return Err(format!(
            "只允许签验 L3/L4 venue 证据，当前声明为 {}",
            identity.level.as_str()
        ));
    }
    Ok(identity)
}

fn read_seed(path: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 Ed25519 种子文件失败 {}: {error}", path.display()))?;
    let bytes = decode_hex_32(text.trim(), "私钥种子")?;
    bytes
        .try_into()
        .map_err(|_| "Ed25519 私钥种子必须正好 32 字节".into())
}

fn read_trusted_key(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取受信任 Ed25519 公钥失败 {}: {error}", path.display()))?;
    let public_key = text.trim().to_ascii_lowercase();
    decode_hex_32(&public_key, "公钥")?;
    Ok(public_key)
}

fn decode_hex_32(value: &str, name: &str) -> Result<Vec<u8>, String> {
    if value.len() != 64 {
        return Err(format!("Ed25519 {name} 必须是 64 个十六进制字符"));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| format!("Ed25519 {name} 不是合法十六进制"))
        })
        .collect()
}

fn signature_message(bundle: &EvidenceBundle) -> Result<Vec<u8>, String> {
    Ok(bundle.to_json()?.into_bytes())
}

fn write_signature_file(path: &Path, signature_hex: &str) -> Result<(), String> {
    let envelope = SignatureEnvelope {
        schema_version: SIGNATURE_SCHEMA_VERSION,
        algorithm: "Ed25519".into(),
        signature_hex: signature_hex.into(),
    };
    let payload = serde_json::to_vec_pretty(&envelope)
        .map_err(|error| format!("签名信封序列化失败: {error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建签名文件失败 {}: {error}", path.display()))?;
    if let Err(error) = file.write_all(&payload) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("写入签名文件失败 {}: {error}", path.display()));
    }
    Ok(())
}

fn read_signature_file(path: &Path) -> Result<SignatureEnvelope, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取签名信封失败 {}: {error}", path.display()))?;
    let envelope: SignatureEnvelope =
        serde_json::from_str(&payload).map_err(|error| format!("签名信封格式非法: {error}"))?;
    if envelope.schema_version != SIGNATURE_SCHEMA_VERSION || envelope.algorithm != "Ed25519" {
        return Err("签名信封版本或算法不受支持".into());
    }
    Ok(envelope)
}

fn sign(bundle_path: &Path, key_path: &Path, signature_path: &Path) -> Result<(), String> {
    let bundle = read_bundle(bundle_path)?;
    let identity = venue_identity(&bundle)?;
    let seed = read_seed(key_path)?;
    let message = signature_message(&bundle)?;
    let signature =
        qx_plugin::sign_detached_ed25519(&message, &seed).map_err(|error| error.to_string())?;
    write_signature_file(signature_path, &signature.signature_hex)?;
    print_result(
        "signed",
        &identity,
        Some(signature_path),
        Some(signature.public_key_hex),
    )?;
    Ok(())
}

fn verify(
    bundle_path: &Path,
    signature_path: &Path,
    trusted_key_path: &Path,
) -> Result<(), String> {
    let bundle = read_bundle(bundle_path)?;
    let identity = venue_identity(&bundle)?;
    let envelope = read_signature_file(signature_path)?;
    let trusted_key = read_trusted_key(trusted_key_path)?;
    let message = signature_message(&bundle)?;
    qx_plugin::verify_detached_ed25519(&message, &trusted_key, &envelope.signature_hex)
        .map_err(|error| error.to_string())?;
    print_result("signature-verified", &identity, Some(signature_path), None)?;
    Ok(())
}

fn print_result(
    operation: &str,
    identity: &VenueIdentity<'_>,
    signature_path: Option<&Path>,
    public_key_hex: Option<String>,
) -> Result<(), String> {
    let result = SignatureResult {
        ok: true,
        operation,
        signature_algorithm: "Ed25519",
        generation: "venue",
        capability_level: identity.level.as_str(),
        venue_id: identity.venue_id,
        window: identity.window,
        signature_file: signature_path.map(|path| path.display().to_string()),
        public_key_hex,
    };
    let json =
        serde_json::to_string(&result).map_err(|error| format!("序列化签名结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

pub(crate) fn run(args: EvidenceArgs) -> Result<(), String> {
    match args.action {
        Some(EvidenceCommand::Sign {
            bundle,
            signing_key_file,
            signature_file,
        }) => sign(&bundle, &signing_key_file, &signature_file),
        Some(EvidenceCommand::Verify {
            bundle,
            signature_file,
            trusted_key_file,
        }) => verify(&bundle, &signature_file, &trusted_key_file),
        Some(EvidenceCommand::PackVenueAcceptance {
            acceptance_dir,
            operator,
        }) => crate::venue_evidence::pack_acceptance(&acceptance_dir, &operator),
        Some(EvidenceCommand::VerifyRun { bundle, recompute }) => {
            crate::run_evidence_verify::verify_run_bundle(&bundle, recompute)
        }
        Some(EvidenceCommand::ExportRun {
            bundle,
            output_dir,
            runtime_config,
            market_spec,
            build_attestation,
        }) => crate::run_evidence_export::export_run_bundle(
            &bundle,
            &output_dir,
            runtime_config.as_deref(),
            market_spec.as_deref(),
            build_attestation.as_deref(),
        ),
        Some(EvidenceCommand::PackMultiAccountRun {
            result,
            spec,
            output_dir,
        }) => {
            crate::run_evidence_multi_account::pack_multi_account_run(&result, &spec, &output_dir)
        }
        Some(EvidenceCommand::VerifyMultiAccountRun { bundle_dir }) => {
            crate::run_evidence_multi_account::verify_multi_account_run(&bundle_dir)
        }
        Some(EvidenceCommand::PackMultiMarketRun { artifact }) => {
            let path =
                crate::run_evidence_multi_market::write_multi_market_run_evidence(&artifact, None)?;
            println!("{}", path.display());
            Ok(())
        }
        Some(EvidenceCommand::RecomputeMultiMarketExperiment { artifact }) => {
            crate::run_evidence_experiment_recompute::recompute_multi_market_experiment(&artifact)
        }
        Some(EvidenceCommand::RecomputeMultiMarketDepthExperiment { artifact }) => {
            crate::run_evidence_experiment_recompute::recompute_multi_market_depth_experiment(
                &artifact,
            )
        }
        Some(EvidenceCommand::RecomputeMultiLegExperiment { artifact }) => {
            crate::run_evidence_experiment_recompute::recompute_multi_leg_experiment(&artifact)
        }
        Some(EvidenceCommand::PackMultiLegExperiment {
            artifact,
            output_dir,
        }) => {
            crate::run_evidence_experiment_recompute::pack_multi_leg_experiment(
                &artifact,
                &output_dir,
            )?;
            println!(
                "[qx-cli · CLI] portable experiment package: {}",
                output_dir.display()
            );
            Ok(())
        }
        Some(EvidenceCommand::VerifyMultiLegExperiment {
            bundle_dir,
            recompute,
        }) => crate::run_evidence_experiment_recompute::verify_multi_leg_experiment_bundle(
            &bundle_dir,
            recompute,
        ),
        Some(EvidenceCommand::PackMultiMarketExperiment {
            artifact,
            output_dir,
        }) => {
            crate::run_evidence_experiment_recompute::pack_multi_market_experiment(
                &artifact,
                &output_dir,
                false,
            )?;
            println!(
                "[qx-cli · CLI] portable experiment package: {}",
                output_dir.display()
            );
            Ok(())
        }
        Some(EvidenceCommand::PackMultiMarketDepthExperiment {
            artifact,
            output_dir,
        }) => {
            crate::run_evidence_experiment_recompute::pack_multi_market_experiment(
                &artifact,
                &output_dir,
                true,
            )?;
            println!(
                "[qx-cli · CLI] portable experiment package: {}",
                output_dir.display()
            );
            Ok(())
        }
        Some(EvidenceCommand::VerifyMultiMarketExperiment {
            bundle_dir,
            recompute,
        }) => crate::run_evidence_experiment_recompute::verify_multi_market_experiment_bundle(
            &bundle_dir,
            recompute,
        ),
        None => Err("请指定 evidence 签验/导出/打包命令或实验复算命令".into()),
    }
}

pub(crate) fn dispatch(args: EvidenceArgs) {
    if let Err(error) = run(args) {
        eprintln!("[qx-cli · CLI] EvidenceBundle 签验失败: {error}");
        std::process::exit(2);
    }
}
