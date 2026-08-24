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

use sqlx_pg_copy_helper::{
    BufferSize, PGCopyTable, PgFlattenable, generate_create_table_string, insert_copy_row_values,
};
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

/// Struct from the crate's usage example: renames a field's column via `#[pg_copy(name = ...)]`.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "events")]
struct Event {
    id: i64,
    label: Option<String>,
    #[pg_copy(name = "ts")]
    ts_tz: chrono::DateTime<chrono::Utc>,
}

/// Conversion helpers for custom domain types, used by `Reading` below.
mod my_mod {
    #[derive(Debug, Clone)]
    pub struct MyType(pub i64);

    pub fn to_pg_value(value: &MyType) -> i64 {
        value.0
    }

    #[derive(Debug, Clone)]
    pub struct OtherType(pub String);

    pub fn try_to_pg_value(value: &OtherType) -> sqlx_pg_copy_helper::Result<f64> {
        Ok(value.0.parse::<f64>()?)
    }
}

/// Embedded key — no table of its own, used only via flatten.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(wrapped)]
struct ReadingKey {
    device_id: i64,
    sensor: String,
}

/// Directly insertable row exercising every column-mapping feature at once: a plain field, an
/// explicit `sql_type` override, an `IpNetwork` split across two columns, an infallible `convert`,
/// a fallible `try_convert`, a `skip`ped field, and a `flatten`ed embedded key.
#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "readings")]
struct Reading {
    id: i64,
    label: Option<String>,
    ts: chrono::NaiveDateTime,
    #[pg_copy(sql_type = "FLOAT8")]
    raw_value: f64,
    #[pg_copy(name = "net_inet", sql_type = "INET")]
    #[pg_copy(name = "net_cidr", sql_type = "CIDR")]
    network: Option<ipnetwork::IpNetwork>,
    #[pg_copy(sql_type = "INT8", convert = "my_mod::to_pg_value")]
    custom: my_mod::MyType,
    #[pg_copy(sql_type = "FLOAT8", try_convert = "my_mod::try_to_pg_value")]
    other: my_mod::OtherType,
    #[pg_copy(skip)]
    internal_flag: bool,
    #[pg_copy(flatten)]
    key: ReadingKey,
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

#[tokio::test]
async fn test_event_insert_and_fetch() {
    let (_container, pool) = start_pg::<Event>().await;

    let schema = generate_create_table_string::<Event>();
    assert_eq!(
        "CREATE TABLE IF NOT EXISTS events (id int8 NOT NULL, label varchar, ts timestamptz NOT NULL)",
        schema
    );

    let rows = vec![
        Event {
            id: 1,
            label: Some("startup".to_string()),
            ts_tz: chrono::DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        },
        Event {
            id: 2,
            label: None,
            ts_tz: chrono::DateTime::parse_from_rfc3339("2024-01-15T13:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query("SELECT id, label, ts FROM events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    insta::assert_debug_snapshot!(fetched);
}

/// Verifies column names/types/order for a row exercising every mapping feature at once.
#[test]
fn test_reading_fields() {
    use Reading as Row;

    assert_eq!(Row::table_name(), "readings");
    let fields = Row::fields();

    let expected: Vec<(&str, &str)> = vec![
        ("id", "int8"),
        ("label", "varchar"),
        ("ts", "timestamp"),
        ("raw_value", "float8"),
        ("net_inet", "inet"),
        ("net_cidr", "cidr"),
        ("custom", "int8"),    // MyType → INT8 via convert = "my_mod::to_pg_value"
        ("other", "float8"),   // OtherType → FLOAT8 via try_convert = "my_mod::try_to_pg_value"
        ("device_id", "int8"), // flattened from ReadingKey
        ("sensor", "varchar"), // flattened from ReadingKey
    ];
    let actual: Vec<(&str, &str)> = fields.iter().map(|f| (f.name, f.sql_type.name())).collect();

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn test_reading_insert_and_fetch() {
    let (_container, pool) = start_pg::<Reading>().await;

    let schema = generate_create_table_string::<Reading>();
    assert_eq!(
        schema,
        "CREATE TABLE IF NOT EXISTS readings (id int8 NOT NULL, label varchar, ts timestamp NOT NULL, \
         raw_value float8 NOT NULL, net_inet inet, net_cidr cidr, custom int8 NOT NULL, other float8 NOT NULL, \
         device_id int8 NOT NULL, sensor varchar NOT NULL)"
    );

    let rows = vec![
        Reading {
            id: 1,
            label: Some("sensor-a".to_string()),
            ts: chrono::NaiveDate::from_ymd_opt(2024, 1, 15)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            raw_value: 1.5,
            network: Some(
                ipnetwork::IpNetwork::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 0)),
                    8,
                )
                .unwrap(),
            ),
            custom: my_mod::MyType(42),
            other: my_mod::OtherType("3.5".to_string()),
            internal_flag: true,
            key: ReadingKey {
                device_id: 100,
                sensor: "temp".to_string(),
            },
        },
        Reading {
            id: 2,
            label: None,
            ts: chrono::NaiveDate::from_ymd_opt(2024, 1, 15)
                .unwrap()
                .and_hms_opt(13, 0, 0)
                .unwrap(),
            raw_value: 2.5,
            network: None,
            custom: my_mod::MyType(7),
            other: my_mod::OtherType("-1.25".to_string()),
            internal_flag: false,
            key: ReadingKey {
                device_id: 200,
                sensor: "humidity".to_string(),
            },
        },
    ];

    insert_copy_row_values(&pool, rows, BufferSize::Default)
        .await
        .unwrap();

    let fetched = sqlx::query(
        "SELECT id, label, ts, raw_value, net_inet, net_cidr, custom, other, device_id, sensor \
         FROM readings ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    insta::assert_debug_snapshot!(fetched);
}
