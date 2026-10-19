use super::storage_adopt_args::PostgresSchemaAdoptArgs;

#[cfg(feature = "postgres")]
pub(crate) fn dispatch_postgres_schema_adoption(args: PostgresSchemaAdoptArgs) {
    let dsn = match std::env::var(&args.dsn_env) {
        Ok(dsn) => dsn,
        Err(_) => {
            eprintln!(
                "[qx-cli · CLI] 未设置 PostgreSQL DSN 环境变量 {}",
                args.dsn_env
            );
            std::process::exit(2);
        }
    };
    match qx_storage::adopt_legacy_postgres_schema(&dsn, &args.schema) {
        Ok(version) => {
            println!(
                "[存储迁移 · PostgreSQL] schema={} adopted_version={version}",
                args.schema
            )
        }
        Err(error) => {
            eprintln!("[qx-cli · CLI] PostgreSQL schema 认领失败: {error:?}");
            std::process::exit(2);
        }
    }
}

#[cfg(not(feature = "postgres"))]
pub(crate) fn dispatch_postgres_schema_adoption(_args: PostgresSchemaAdoptArgs) {
    eprintln!("[qx-cli · CLI] storage-adopt-postgres 需要使用 --features postgres 构建 qx-cli");
    std::process::exit(2);
}
