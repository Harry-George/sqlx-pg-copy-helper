//! Integration tests for the `#[derive(PGCopyTable)]` proc-macro.
//!
//! These live in `tests/` (compiled as a separate crate) so that the generated
//! code's `::sqlx_pg_copy_helper::` paths resolve correctly.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::shadow_unrelated,
    clippy::trivially_copy_pass_by_ref,
    clippy::type_complexity,
    clippy::unwrap_used,
    clippy::expect_used
)]

use sqlx_pg_copy_helper::{BufferSize, PGCopyTable, PgFlattenable, insert_copy_row_values};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres as PostgresImage;

// ─── Conversion helpers ───────────────────────────────────────────────────────

/// Parses a decimal string into an `i64`, used to exercise the `try_convert` key.
fn parse_int(s: &str) -> sqlx_pg_copy_helper::Result<i64> {
    Ok(s.parse::<i64>()?)
}

/// Doubles an `i64`, used to exercise the infallible `convert` key.
fn double_int(x: &i64) -> i64 {
    x * 2
}

// ─── Test structs ─────────────────────────────────────────────────────────────

/// All common field types with no annotations — exercises the full inference path.
/// The `metadata` field has `#[pg_copy(skip)]` and must not appear in any SQL column.
#[derive(Debug, Clone, PartialEq, PGCopyTable)]
#[pg_copy(table = "inferred_rows")]
struct InferredRow {
    id: i64,
    label: Option<String>,
    value: f64,
    #[pg_copy(name = "ts")]
    #[pg_copy]
    ts_tz: chrono::DateTime<chrono::Utc>,
    #[pg_copy(skip)]
    metadata: String,
}

/// Exercises explicit `name` and `sql_type` overrides.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "override_rows")]
struct OverrideRow {
    id: i64,
    /// Column renamed; `sql_type` should still be inferred as TEXT.
    #[pg_copy(name = "full_name")]
    name: String,
    /// `sql_type` explicitly set to INT4 (differs from the inferred INT8).
    #[pg_copy(sql_type = "INT4")]
    score: i32,
}

/// Exercises `try_convert`: `count` is a `String` parsed to `INT8` via `parse_int` (fallible).
/// Exercises `convert`: `id_copy` doubles `id` via `double_int` (infallible).
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "convert_rows")]
struct ConvertRow {
    id: i64,
    #[pg_copy(sql_type = "INT8", try_convert = "parse_int")]
    count: String,
    #[pg_copy(name = "double_id", sql_type = "INT8", convert = "double_int")]
    id_copy: i64,
}

/// `IpNetwork` with no annotation: auto-infers CIDR type and auto-wraps with `IpNetworkCidr`.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "auto_network_rows")]
struct AutoNetworkRow {
    id: i64,
    network: Option<ipnetwork::IpNetwork>,
}

/// Key struct for testing `flatten`.  Wrapped-only: cannot be inserted directly.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(wrapped)]
struct FlatKey {
    category: String,
    subcategory: Option<String>,
}

/// Outer struct that flattens `FlatKey` inline — `key` disappears and its two columns appear
/// at the same position in the column list.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "flat_rows")]
struct FlatRow {
    id: i64,
    #[pg_copy(flatten)]
    key: FlatKey,
    score: f64,
}

/// `IpNetwork` mapped to explicit column names and `sql_types` (INET and CIDR) without `convert`.
/// The macro auto-detects the `IpNetwork` type and inserts the `IpNetworkCidr` wrapper.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "network_rows")]
struct NetworkRow {
    id: i64,
    inet: Option<std::net::IpAddr>,
    cidr: Option<cidr::IpCidr>,
    /// One field mapped to two columns — wrapping is injected automatically.
    #[pg_copy(name = "net_inet", sql_type = "INET")]
    #[pg_copy(name = "net_cidr", sql_type = "CIDR")]
    network: Option<ipnetwork::IpNetwork>,
}

/// `IpNetwork` mapped to explicit column names and `sql_types` (INET and CIDR) without `convert`.
/// The macro auto-detects the `IpNetwork` type and inserts the `IpNetworkCidr` wrapper.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "other_types_rows")]
struct OtherTypesRow {
    pub day: chrono::NaiveDate,
    pub float_value: f64,
    pub integer: i64,
}

#[test]
fn test_inferred_rows() {
    use InferredRow as Row;

    assert_eq!(Row::table_name(), "inferred_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("label", "varchar"),
        ("value", "float8"),
        ("ts", "timestamptz"),
        ("ts_tz", "timestamptz"),
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_convert_rows() {
    use ConvertRow as Row;

    assert_eq!(Row::table_name(), "convert_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("count", "int8"),     // String → INT8 via try_convert = "parse_int"
        ("double_id", "int8"), // i64   → INT8 via convert    = "double_int"
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_auto_network_rows() {
    use AutoNetworkRow as Row;

    assert_eq!(Row::table_name(), "auto_network_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("network", "cidr"), // IpNetwork infers to CIDR with no annotation
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_network_rows() {
    use NetworkRow as Row;

    assert_eq!(Row::table_name(), "network_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("inet", "inet"),
        ("cidr", "cidr"),
        ("net_inet", "inet"), // IpNetwork with explicit sql_type = "INET"
        ("net_cidr", "cidr"), // IpNetwork with explicit sql_type = "CIDR"
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_other_types_rows() {
    use OtherTypesRow as Row;

    assert_eq!(Row::table_name(), "other_types_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("day", "date"),
        ("float_value", "float8"),
        ("integer", "int8"),
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_override_rows() {
    use OverrideRow as Row;

    assert_eq!(Row::table_name(), "override_rows");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> =
        vec![("id", "int8"), ("full_name", "varchar"), ("score", "int4")];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

/// Verifies that `flatten` inlines the nested struct's columns at the right position.
#[test]
fn test_flatten_fields() {
    use FlatRow as Row;

    assert_eq!(Row::table_name(), "flat_rows");
    let fields = Row::fields();

    // `key: FlatKey` disappears; its two columns appear between `id` and `score`.
    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("category", "varchar"),
        ("subcategory", "varchar"),
        ("score", "float8"),
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

/// Verifies that getter functions work correctly — values are read through the nested accessor.
#[test]
fn test_flatten_getters() {
    use sqlx_pg_copy_helper::PgFlattenable as _;

    let row = FlatRow {
        id: 99,
        key: FlatKey {
            category: "alpha".to_string(),
            subcategory: Some("beta".to_string()),
        },
        score: 1.5,
    };

    let fields = FlatRow::fields();
    // name → index: id=0, category=1, subcategory=2, score=3
    let category_val = (fields[1].getter_func)(&row).unwrap();
    let score_val = (fields[3].getter_func)(&row).unwrap();

    assert!(
        category_val
            .as_sql()
            .to_sql_checked(
                &tokio_postgres::types::Type::VARCHAR,
                &mut bytes::BytesMut::new()
            )
            .is_ok()
    );
    assert!(
        score_val
            .as_sql()
            .to_sql_checked(
                &tokio_postgres::types::Type::FLOAT8,
                &mut bytes::BytesMut::new()
            )
            .is_ok()
    );
}

async fn start_pg<T: PGCopyTable>() -> (impl std::any::Any, sqlx::Pool<sqlx::Postgres>) {
    let container = PostgresImage::default().start().await.unwrap();

    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();

    let pool = sqlx::PgPool::connect(&format!(
        "postgres://postgres:postgres@{host}:{port}/postgres"
    ))
    .await
    .expect("connect to postgres");

    let cols = T::fields()
        .iter()
        .map(|f| format!("{} {}", f.name, f.sql_type.name()))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE IF NOT EXISTS {} ({cols})",
        T::table_name()
    )))
    .execute(&pool)
    .await
    .unwrap();

    (container, pool)
}

#[tokio::test]
async fn test_derive_inferred_insert_and_fetch() {
    let (_container, pool) = start_pg::<InferredRow>().await;

    let base_tz = chrono::DateTime::parse_from_rfc3339("2024-06-01T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let rows = vec![
        InferredRow {
            id: 1,
            label: Some("hello".to_string()),
            value: 1.5,
            ts_tz: base_tz,
            metadata: "not persisted".to_string(),
        },
        InferredRow {
            id: 2,
            label: None,
            value: 2.5,
            ts_tz: base_tz + chrono::Duration::hours(1),
            metadata: "also not persisted".to_string(),
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT id, label, value, ts, ts_tz FROM inferred_rows ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

#[tokio::test]
async fn test_derive_convert_insert_and_fetch() {
    let (_container, pool) = start_pg::<ConvertRow>().await;

    let rows = vec![
        ConvertRow {
            id: 1,
            count: "42".to_string(),
            id_copy: 1,
        },
        ConvertRow {
            id: 2,
            count: "1000".to_string(),
            id_copy: 2,
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT id, count FROM convert_rows ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

#[tokio::test]
async fn test_derive_auto_ip_network_insert_and_fetch() {
    let (_container, pool) = start_pg::<AutoNetworkRow>().await;

    let rows = vec![
        AutoNetworkRow {
            id: 1,
            network: Some(
                ipnetwork::IpNetwork::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 0)),
                    8,
                )
                .unwrap(),
            ),
        },
        AutoNetworkRow {
            id: 2,
            network: None,
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT id, network FROM auto_network_rows ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

#[tokio::test]
async fn test_derive_ip_network_insert_and_fetch() {
    let (_container, pool) = start_pg::<NetworkRow>().await;

    let rows = vec![
        NetworkRow {
            id: 1,
            inet: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1))),
            cidr: Some(cidr::IpCidr::V4(
                cidr::Ipv4Cidr::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
            )),
            network: Some(
                ipnetwork::IpNetwork::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 0)),
                    24,
                )
                .unwrap(),
            ),
        },
        NetworkRow {
            id: 2,
            inet: None,
            cidr: None,
            network: None,
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched =
        sqlx::query("SELECT id, inet, cidr, net_inet, net_cidr FROM network_rows ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

#[tokio::test]
async fn test_other_types_insert_and_fetch() {
    let (_container, pool) = start_pg::<OtherTypesRow>().await;

    let rows = vec![
        OtherTypesRow {
            day: chrono::NaiveDate::from_ymd_opt(2024, 6, 1).unwrap(),
            float_value: 1.5,
            integer: 42,
        },
        OtherTypesRow {
            day: chrono::NaiveDate::from_ymd_opt(2024, 6, 2).unwrap(),
            float_value: 2.5,
            integer: 1000,
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT day, float_value FROM other_types_rows ORDER BY float_value")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

#[tokio::test]
async fn test_derive_flatten_insert_and_fetch() {
    let (_container, pool) = start_pg::<FlatRow>().await;

    let rows = vec![
        FlatRow {
            id: 1,
            key: FlatKey {
                category: "widgets".to_string(),
                subcategory: Some("small".to_string()),
            },
            score: 9.5,
        },
        FlatRow {
            id: 2,
            key: FlatKey {
                category: "gadgets".to_string(),
                subcategory: None,
            },
            score: 4.0,
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT id, category, subcategory, score FROM flat_rows ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}
