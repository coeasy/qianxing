use super::*;

/// Explicitly record a known legacy PostgreSQL schema in the migration ledger.
///
/// This is intentionally separate from `connect`: it never creates a missing target
/// schema and only adopts a schema whose catalog matches one of this crate's migrations.
pub fn adopt_legacy_postgres_schema(dsn: &str, schema: &str) -> Result<u32, StorageError> {
    if dsn.trim().is_empty() || schema.trim().is_empty() {
        return Err(StorageError::Conflict(
            "PostgreSQL DSN 和 schema 名称不能为空".into(),
        ));
    }
    let mut config: postgres::Config = dsn.parse().map_err(pg_error)?;
    config.connect_timeout(CONNECT_TIMEOUT);
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|error| StorageError::Io(format!("PostgreSQL TLS 初始化失败: {error}")))?;
    let connector = postgres_native_tls::MakeTlsConnector::new(tls);
    let mut client = config.connect(connector).map_err(pg_error)?;
    configure_client(&mut client)?;
    adopt_legacy_postgres_client(&mut client, schema)
}

fn adopt_legacy_postgres_client(client: &mut Client, schema: &str) -> Result<u32, StorageError> {
    let mut transaction = client.transaction().map_err(pg_error)?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&schema],
        )
        .map_err(pg_error)?;
    let schema_exists: bool = transaction
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)",
            &[&schema],
        )
        .map_err(pg_error)?
        .get(0);
    if !schema_exists {
        return Err(StorageError::Conflict(format!(
            "PostgreSQL schema {schema:?} 不存在，拒绝认领"
        )));
    }
    let schema_path = format!("\"{}\"", schema.replace('"', "\"\""));
    transaction
        .query_one(
            "SELECT set_config('search_path', $1, true)",
            &[&schema_path],
        )
        .map_err(pg_error)?;
    let ledger_exists: bool = transaction
        .query_one(
            "SELECT to_regclass('qx_schema_migrations') IS NOT NULL",
            &[],
        )
        .map_err(pg_error)?
        .get(0);
    if ledger_exists {
        return Err(StorageError::Conflict(
            "PostgreSQL schema 已有迁移账本，拒绝重复认领".into(),
        ));
    }

    let control_schema = format!(
        "qx_adopt_verify_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| StorageError::Io(format!("系统时间不可用: {error}")))?
            .as_nanos()
    );
    transaction
        .batch_execute(&format!("CREATE SCHEMA \"{control_schema}\""))
        .map_err(pg_error)?;

    let adoption_version = (|| -> Result<u32, StorageError> {
        transaction
            .query_one(
                "SELECT set_config('search_path', $1, true)",
                &[&control_schema],
            )
            .map_err(pg_error)?;
        transaction
            .batch_execute(crate::schema_migrations::POSTGRES_V1_SCHEMA)
            .map_err(pg_error)?;
        let expected_v1 = postgres_schema_fingerprint(&mut transaction, &control_schema)?;
        transaction
            .batch_execute(crate::schema_migrations::POSTGRES_V2_SCHEMA)
            .map_err(pg_error)?;
        let expected_v2 = postgres_schema_fingerprint(&mut transaction, &control_schema)?;

        transaction
            .query_one(
                "SELECT set_config('search_path', $1, true)",
                &[&schema_path],
            )
            .map_err(pg_error)?;
        let actual = postgres_schema_fingerprint(&mut transaction, schema)?;
        let version = if actual == expected_v1 {
            1
        } else if actual == expected_v2 {
            2
        } else {
            return Err(StorageError::Conflict(format!(
                "PostgreSQL schema {schema:?} 与已知 v1/v2 迁移结构不完全一致，拒绝认领"
            )));
        };
        transaction
            .batch_execute(crate::schema_migrations::CREATE_SCHEMA_MIGRATIONS_SQL)
            .map_err(pg_error)?;
        transaction
            .execute(
                "INSERT INTO qx_schema_migrations(version, applied_ts) VALUES ($1, 'manual-adoption')",
                &[&"1"],
            )
            .map_err(pg_error)?;
        if version == 2 {
            transaction
                .execute(
                    "INSERT INTO qx_schema_migrations(version, applied_ts) VALUES ($1, 'manual-adoption')",
                    &[&"2"],
                )
                .map_err(pg_error)?;
        }
        Ok(version)
    })();

    // The verification schema and ledger are in the same transaction. On any failure,
    // dropping the transaction rolls both back, leaving the target untouched.
    let adopted_version = adoption_version?;
    transaction
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS \"{control_schema}\" CASCADE"
        ))
        .map_err(pg_error)?;
    transaction.commit().map_err(pg_error)?;
    Ok(adopted_version)
}

fn postgres_schema_fingerprint<C: GenericClient>(
    client: &mut C,
    schema: &str,
) -> Result<Vec<String>, StorageError> {
    let mut signature = Vec::new();
    for row in client
        .query(
            "SELECT c.relname, c.relkind::text, c.relpersistence::text
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
             ORDER BY c.relname",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        signature.push(format!(
            "relation:{:?}:{}:{}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        ));
    }
    for row in client
        .query(
            "SELECT c.relname, a.attnum, a.attname, format_type(a.atttypid, a.atttypmod),
                    a.attnotnull, COALESCE(pg_get_expr(d.adbin, d.adrelid), ''),
                    a.attidentity::text, a.attgenerated::text
             FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p')
               AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY c.relname, a.attnum",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        signature.push(format!(
            "column:{:?}:{}:{:?}:{:?}:{:?}:{}:{}:{}",
            row.get::<_, String>(0),
            row.get::<_, i16>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3),
            row.get::<_, bool>(4),
            row.get::<_, String>(5),
            row.get::<_, String>(6),
            row.get::<_, String>(7)
        ));
    }
    for row in client
        .query(
            "SELECT c.relname, con.conname, con.contype::text, pg_get_constraintdef(con.oid, true)
             FROM pg_constraint con
             JOIN pg_class c ON c.oid = con.conrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1
             ORDER BY c.relname, con.conname",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        signature.push(format!(
            "constraint:{:?}:{:?}:{}:{:?}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3)
        ));
    }
    for row in client
        .query(
            "SELECT index_class.relname, table_class.relname, pg_get_indexdef(index_class.oid)
             FROM pg_class index_class
             JOIN pg_namespace n ON n.oid = index_class.relnamespace
             JOIN pg_index i ON i.indexrelid = index_class.oid
             JOIN pg_class table_class ON table_class.oid = i.indrelid
             WHERE n.nspname = $1
             ORDER BY index_class.relname",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        let definition: String = row.get(2);
        let quoted_schema = schema.replace('"', "\"\"");
        let normalized = definition
            .replace(&format!("\"{quoted_schema}\"."), "")
            .replace(&format!("{schema}."), "");
        signature.push(format!(
            "index:{:?}:{:?}:{:?}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            normalized
        ));
    }
    for row in client
        .query(
            "SELECT c.relname, t.tgname, t.tgenabled::text, pg_get_triggerdef(t.oid, true)
             FROM pg_trigger t
             JOIN pg_class c ON c.oid = t.tgrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND NOT t.tgisinternal
             ORDER BY c.relname, t.tgname",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        signature.push(format!(
            "trigger:{:?}:{:?}:{}:{:?}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3)
        ));
    }
    for row in client
        .query(
            "SELECT c.relname, p.polname, p.polcmd::text, p.polpermissive,
                    pg_get_expr(p.polqual, p.polrelid),
                    pg_get_expr(p.polwithcheck, p.polrelid), p.polroles::text
             FROM pg_policy p
             JOIN pg_class c ON c.oid = p.polrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1
             ORDER BY c.relname, p.polname",
            &[&schema],
        )
        .map_err(pg_error)?
    {
        signature.push(format!(
            "policy:{:?}:{:?}:{}:{}:{:?}:{:?}:{}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, bool>(3),
            row.get::<_, Option<String>>(4),
            row.get::<_, Option<String>>(5),
            row.get::<_, String>(6)
        ));
    }
    Ok(signature)
}
