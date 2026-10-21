//! Shared SQL schema migration ledger and ordered migration plan.

use crate::StorageError;

pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 2;
#[cfg(feature = "sqlite")]
pub(crate) const SQLITE_V1_SCHEMA: &str = include_str!("../migrations/sqlite/0001_schema.sql");
#[cfg(feature = "sqlite")]
pub(crate) const SQLITE_V2_SCHEMA: &str = include_str!("../migrations/sqlite/0002_schema.sql");
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_V1_SCHEMA: &str = include_str!("../migrations/postgres/0001_schema.sql");
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_V2_SCHEMA: &str = include_str!("../migrations/postgres/0002_schema.sql");
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) const CREATE_SCHEMA_MIGRATIONS_SQL: &str =
    "CREATE TABLE IF NOT EXISTS qx_schema_migrations (
         version TEXT PRIMARY KEY NOT NULL,
         applied_ts TEXT NOT NULL
     );";

const MIGRATION_VERSIONS: [&str; 2] = ["1", "2"];

/// Return unapplied migrations in order. An unversioned database is bootstrappable only when it
/// has no business tables; databases already at v1 can be upgraded transactionally to v2.
pub(crate) fn pending_schema_versions(
    versions: &[String],
    has_existing_schema_tables: bool,
) -> Result<Vec<&'static str>, StorageError> {
    let mut parsed = versions
        .iter()
        .map(|version| {
            version.parse::<u32>().map_err(|error| {
                StorageError::Conflict(format!("存储 schema 版本非法 {version:?}: {error}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    parsed.sort_unstable();
    parsed.dedup();
    if parsed.len() != versions.len() {
        return Err(StorageError::Conflict(
            "存储 schema 版本台账含重复版本".into(),
        ));
    }

    if parsed.is_empty() && has_existing_schema_tables {
        return Err(StorageError::Conflict(
            "存储 schema 台账缺失，但数据库已存在业务表；需先执行显式迁移".into(),
        ));
    }

    for (index, version) in parsed.iter().enumerate() {
        if *version > CURRENT_SCHEMA_VERSION {
            return Err(StorageError::Conflict(format!(
                "不支持的存储 schema 版本 {version}，当前支持版本为 {CURRENT_SCHEMA_VERSION}"
            )));
        }
        let expected = index as u32 + 1;
        if *version != expected {
            return Err(StorageError::Conflict(format!(
                "存储 schema 迁移序列不连续：期望版本 {expected}，实际 {version}"
            )));
        }
        debug_assert!(index < MIGRATION_VERSIONS.len());
    }

    Ok(MIGRATION_VERSIONS[parsed.len()..].to_vec())
}

pub(crate) fn validate_schema_versions(versions: &[String]) -> Result<(), StorageError> {
    if versions.is_empty() {
        return Err(StorageError::Conflict(
            "存储 schema 版本台账为空且未能初始化".into(),
        ));
    }
    pending_schema_versions(versions, false).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_plan_upgrades_v1_and_rejects_unknown_or_unversioned_data() {
        assert_eq!(pending_schema_versions(&[], false).unwrap(), vec!["1", "2"]);
        assert_eq!(
            pending_schema_versions(&["1".into()], true).unwrap(),
            vec!["2"]
        );
        assert!(pending_schema_versions(&["1".into(), "2".into()], true)
            .unwrap()
            .is_empty());
        assert!(pending_schema_versions(&[], true).is_err());
        assert!(pending_schema_versions(&["2".into()], true).is_err());
        assert!(pending_schema_versions(&["1".into(), "3".into()], true).is_err());
        assert!(pending_schema_versions(&["1".into(), "future".into()], true).is_err());
    }
}
