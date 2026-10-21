use clap::{Args, ValueEnum};

#[derive(Args)]
pub(crate) struct GuideArgs {
    #[arg(value_enum)]
    pub(crate) audience: Option<GuideAudience>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GuideAudience {
    Researcher,
    StrategyAuthor,
    Operator,
    Integrator,
}
