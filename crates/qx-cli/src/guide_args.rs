use clap::{Args, ValueEnum};

#[derive(Args)]
pub(crate) struct GuideArgs {
    #[arg(value_enum)]
    pub(crate) audience: Option<GuideAudience>,
    /// Emit the role guide as versioned JSON for scripts and integrations.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GuideAudience {
    Researcher,
    StrategyAuthor,
    Operator,
    Integrator,
}
