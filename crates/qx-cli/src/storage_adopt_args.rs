use clap::Args;

#[derive(Args)]
pub(crate) struct PostgresSchemaAdoptArgs {
    #[arg(long)]
    pub(crate) schema: String,
    /// Environment variable containing the PostgreSQL DSN; credentials stay out of argv.
    #[arg(long, default_value = "QX_POSTGRES_DSN")]
    pub(crate) dsn_env: String,
}
