use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct EvidenceArgs {
    #[command(subcommand)]
    pub(crate) action: Option<EvidenceCommand>,
}

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
    /// Sign an L3/L4 venue EvidenceBundle. The seed is read from a file and never printed.
    #[command(name = "sign")]
    Sign {
        bundle: PathBuf,
        #[arg(long, value_name = "PATH")]
        signing_key_file: PathBuf,
        #[arg(long, value_name = "PATH")]
        signature_file: PathBuf,
    },
    /// Verify an L3/L4 venue EvidenceBundle with a separately trusted public key.
    #[command(name = "verify")]
    Verify {
        bundle: PathBuf,
        #[arg(long, value_name = "PATH")]
        signature_file: PathBuf,
        #[arg(long, value_name = "PATH")]
        trusted_key_file: PathBuf,
    },
    /// Build an L3 venue EvidenceBundle from a passed, non-dry-run external acceptance window.
    #[command(name = "pack-venue-acceptance")]
    PackVenueAcceptance {
        acceptance_dir: PathBuf,
        #[arg(long, value_name = "NAME")]
        operator: String,
    },
    /// Independently verify the local-run evidence bundle and its referenced files.
    #[command(name = "verify-run")]
    VerifyRun {
        bundle: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Export a run EvidenceBundle and all referenced data/artifacts into a portable directory.
    #[command(name = "export-run")]
    ExportRun {
        bundle: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
        #[arg(long, value_name = "PATH")]
        runtime_config: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        market_spec: Option<PathBuf>,
        /// Attested third-party strategy source, locked dependencies, and reproducible build recipe.
        #[arg(long, value_name = "PATH")]
        build_attestation: Option<PathBuf>,
    },
    /// Package a multi-account batch result, its exact specification, and each portable run bundle.
    #[command(name = "pack-multi-account-run")]
    PackMultiAccountRun {
        result: PathBuf,
        #[arg(long, value_name = "PATH")]
        spec: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify and optionally recompute every account in a portable multi-account package.
    #[command(name = "verify-multi-account-run")]
    VerifyMultiAccountRun {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Package a multi-exchange joint account run and each portable market run bundle.
    #[command(name = "pack-multi-account-joint-run")]
    PackMultiAccountJointRun {
        result: PathBuf,
        #[arg(long, value_name = "PATH")]
        spec: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify and optionally recompute a portable multi-exchange joint account package.
    #[command(name = "verify-multi-account-joint-run")]
    VerifyMultiAccountJointRun {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Package a multi-account Tick/OrderBook batch and all portable member run bundles.
    #[command(name = "pack-multi-account-depth-run")]
    PackMultiAccountDepthRun {
        result: PathBuf,
        #[arg(long, value_name = "PATH")]
        spec: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify and optionally recompute every account in a portable depth batch package.
    #[command(name = "verify-multi-account-depth-run")]
    VerifyMultiAccountDepthRun {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Build a local-run EvidenceBundle from a completed multi-market application artifact.
    #[command(name = "pack-multi-market-run")]
    PackMultiMarketRun { artifact: PathBuf },
    /// Recompute every builtin candidate in a single-market Bar experiment.
    #[command(name = "recompute-experiment")]
    RecomputeExperiment { artifact: PathBuf },
    /// Recompute every builtin candidate in a single-market depth experiment.
    #[command(name = "recompute-depth-experiment")]
    RecomputeDepthExperiment { artifact: PathBuf },
    /// Package a completed builtin single-market Bar experiment with portable inputs and artifacts.
    #[command(name = "pack-experiment")]
    PackExperiment {
        artifact: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Package a completed builtin single-market depth experiment with portable inputs and artifacts.
    #[command(name = "pack-depth-experiment")]
    PackDepthExperiment {
        artifact: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify a portable single-market Bar experiment package and optionally recompute its grid.
    #[command(name = "verify-experiment")]
    VerifyExperiment {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Verify a portable single-market depth experiment package and optionally recompute its grid.
    #[command(name = "verify-depth-experiment")]
    VerifyDepthExperiment {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Recompute every builtin candidate in a multi-market Bar experiment.
    #[command(name = "recompute-multi-market-experiment")]
    RecomputeMultiMarketExperiment { artifact: PathBuf },
    /// Recompute every builtin candidate in a multi-market depth experiment.
    #[command(name = "recompute-multi-market-depth-experiment")]
    RecomputeMultiMarketDepthExperiment { artifact: PathBuf },
    /// Recompute every builtin candidate in a multi-leg experiment.
    #[command(name = "recompute-multi-leg-experiment")]
    RecomputeMultiLegExperiment { artifact: PathBuf },
    /// Package a completed builtin multi-leg experiment and all files needed for replay.
    #[command(name = "pack-multi-leg-experiment")]
    PackMultiLegExperiment {
        artifact: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify a portable multi-leg experiment package and optionally recompute its full grid.
    #[command(name = "verify-multi-leg-experiment")]
    VerifyMultiLegExperiment {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
    /// Package a completed builtin multi-market Bar or depth experiment and its replay inputs.
    #[command(name = "pack-multi-market-experiment")]
    PackMultiMarketExperiment {
        artifact: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Package a completed builtin multi-market depth experiment and its replay inputs.
    #[command(name = "pack-multi-market-depth-experiment")]
    PackMultiMarketDepthExperiment {
        artifact: PathBuf,
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Verify a portable multi-market Bar/depth experiment package and optionally recompute its grid.
    #[command(name = "verify-multi-market-experiment")]
    VerifyMultiMarketExperiment {
        bundle_dir: PathBuf,
        #[arg(long)]
        recompute: bool,
    },
}
