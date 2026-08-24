//! Function will insert the given rows into the table using the copy interface

use crate::{Field, PGCopyTable};
use anyhow::Context as _;
use byteorder::{BigEndian, ByteOrder};
use bytes::{BufMut, BytesMut};
use sqlx::postgres::PgPoolCopyExt;
use sqlx::{Pool, Postgres};
use tokio_postgres::types::{IsNull, ToSql, Type};

/// `PostgreSQL` binary COPY file signature
const BINARY_COPY_HEADER: &[u8] = b"PGCOPY\n\xff\r\n\0";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BufferSize {
    Default,
    Fixed(usize),
}

/// Will insert the given rows into the table using the copy interface
pub async fn insert_copy_row_values<T: PGCopyTable>(
    pool: &Pool<Postgres>,
    rows: Vec<T>,
    max_buffer_size: BufferSize,
) -> anyhow::Result<()> {
    match insert_copy_row_values_internal(pool, rows, max_buffer_size).await {
        Ok(()) => Ok(()),
        Err(e) => Err(e.context(format!("Failed to insert data into {}", T::table_name()))),
    }
}

/// Internal function, allows the public one to more easily wrap the error message
#[allow(clippy::decimal_literal_representation)]
async fn insert_copy_row_values_internal<T: PGCopyTable>(
    pool: &Pool<Postgres>,
    rows: Vec<T>,
    max_buffer_size: BufferSize,
) -> anyhow::Result<()> {
    let max_buffer_size = match max_buffer_size {
        BufferSize::Default => 4096,
        BufferSize::Fixed(size) => size,
    };

    if rows.is_empty() {
        return Ok(());
    }

    let fields = T::fields();
    let copy_string = format!(
        "COPY {} ({}) FROM STDIN WITH (FORMAT BINARY)",
        T::table_name(),
        fields.iter().map(|f| f.name).collect::<Vec<_>>().join(",")
    );

    let mut copy = pool.copy_in_raw(&copy_string).await?;

    let mut buf = BytesMut::new();
    buf.put_slice(BINARY_COPY_HEADER);
    buf.put_i32(0); // flags (no OIDs)
    buf.put_i32(0); // header extension area length

    for row in &rows {
        append_binary_row(&mut buf, row, &fields)?;

        if buf.len() > max_buffer_size {
            copy.send(buf.split().freeze()).await?;
        }
    }

    buf.put_i16(-1); // file trailer
    copy.send(buf.freeze()).await?;
    copy.finish().await?;

    Ok(())
}

/// For a given row write all the fields
#[allow(clippy::expect_used)]
fn append_binary_row<T>(buf: &mut BytesMut, row: &T, fields: &[Field<T>]) -> anyhow::Result<()> {
    buf.put_i16(i16::try_from(fields.len()).expect("too many fields"));
    for field in fields {
        write_field(buf, (field.getter_func)(row)?.as_sql(), &field.sql_type)?;
    }
    Ok(())
}

/// For a single field serialise it
fn write_field<T: ToSql + ?Sized>(
    buf: &mut BytesMut,
    value: &T,
    type_: &Type,
) -> anyhow::Result<()> {
    let size_start = buf.len();
    buf.put_i32(0); // length placeholder
    let value_start = buf.len();

    // Serialize the value into the buffer
    let value_result = value
        .to_sql_checked(type_, buf)
        .map_err(sqlx::Error::Encode)?;

    // Get the length, if its null its -1, otherwise its how much was added to the buffer
    let len = match (value_result) {
        IsNull::Yes => -1i32,
        IsNull::No => i32::try_from(buf.len().saturating_sub(value_start))
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
    };

    let buf_len_for_logging = buf.len();

    // Replace our placeholder with the actual length
    BigEndian::write_i32(buf.get_mut(size_start..value_start)
                             .ok_or_else(|| {
                                 anyhow::anyhow!(
                                     "Failed to get mutable slice for size field {size_start}, {value_start}, {buf_len_for_logging}",
                                 )
                             })?, len);

    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::type_complexity,
    clippy::shadow_unrelated
)]
mod tests {
    use super::*;
    use crate::IpNetworkCidr;
    use crate::PgFlattenable as _;
    use crate::field::FieldValue;
    use chrono::Timelike;
    use testcontainers_modules::postgres::Postgres as PostgresImage;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    #[derive(Debug, Clone, PartialEq)]
    struct TestRow {
        id: i64,
        label: Option<String>,
        value: f64,
        ts: chrono::NaiveDateTime,
        ts_tz: chrono::DateTime<chrono::Utc>,
        inet: Option<std::net::IpAddr>,
        cidr: Option<cidr::IpCidr>,
        sqlx_network: Option<sqlx::types::ipnetwork::IpNetwork>,
    }

    impl crate::PgFlattenable for TestRow {
        fn fields() -> Vec<Field<Self>> {
            vec![
                Field {
                    sql_type: Type::INT8,
                    name: "id",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.id))),
                },
                Field {
                    sql_type: Type::TEXT,
                    name: "label",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.label))),
                },
                Field {
                    sql_type: Type::FLOAT8,
                    name: "value",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.value))),
                },
                Field {
                    sql_type: Type::TIMESTAMP,
                    name: "ts",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.ts))),
                },
                Field {
                    sql_type: Type::TIMESTAMPTZ,
                    name: "ts_tz",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.ts_tz))),
                },
                Field {
                    sql_type: Type::INET,
                    name: "inet",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.inet))),
                },
                Field {
                    sql_type: Type::CIDR,
                    name: "cidr",
                    getter_func: Box::new(|r| Ok(FieldValue::Borrowed(&r.cidr))),
                },
                Field {
                    sql_type: Type::INET,
                    name: "sqlx_network_inet",
                    getter_func: Box::new(|r| {
                        Ok(match r.sqlx_network {
                            Some(network) => {
                                FieldValue::Owned(Box::new(Some(IpNetworkCidr(network))))
                            }
                            None => FieldValue::Owned(Box::new(None::<IpNetworkCidr>)),
                        })
                    }),
                },
                Field {
                    sql_type: Type::CIDR,
                    name: "sqlx_network_cidr",
                    getter_func: Box::new(|r| {
                        Ok(match r.sqlx_network {
                            Some(network) => {
                                FieldValue::Owned(Box::new(Some(IpNetworkCidr(network))))
                            }
                            None => FieldValue::Owned(Box::new(None::<IpNetworkCidr>)),
                        })
                    }),
                },
            ]
        }
    }

    impl PGCopyTable for TestRow {
        fn table_name() -> &'static str {
            "test_rows"
        }
    }

    async fn start_pg() -> (impl std::any::Any, sqlx::Pool<Postgres>) {
        let container = PostgresImage::default().start().await.unwrap();

        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let connection_string = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let pool = sqlx::PgPool::connect(&connection_string)
            .await
            .expect("Failed to connect to database");

        let cols = TestRow::fields()
            .iter()
            .map(|f| format!("{} {}", f.name, f.sql_type.name()))
            .collect::<Vec<_>>()
            .join(", ");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS {} ({cols})",
            TestRow::table_name()
        )))
        .execute(&pool)
        .await
        .unwrap();

        (container, pool)
    }

    #[tokio::test]
    async fn test_insert_copy_row_values() {
        let (_container, pool) = start_pg().await;

        let now_tz = chrono::DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let now_naive = now_tz.naive_utc();

        let rows = vec![
            TestRow {
                id: 1,
                label: Some("first".to_string()),
                value: 1.5,
                ts: now_naive,
                ts_tz: now_tz,
                inet: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 0, 1))),
                cidr: Some(cidr::IpCidr::V4(cidr::Ipv4Cidr::new(std::net::Ipv4Addr::new(192, 168, 0, 0), 24).unwrap())),
                sqlx_network: Some(sqlx::types::ipnetwork::IpNetwork::V4(std::net::Ipv4Addr::new(192, 168, 255, 255).into())),
            },
            TestRow {
                id: 2,
                label: None,
                value: 2.5,
                ts: now_naive + chrono::Duration::hours(1),
                ts_tz: now_tz + chrono::Duration::hours(1),
                inet: None,
                cidr: None,
                sqlx_network: None,
            },
            TestRow {
                id: 3,
                label: Some("I am a really really really long string that is over the size of 256 to make sure that endianess is not a problem for byte ordering;I am a really really really long string that is over the size of 256 to make sure that endianess is not a problem for byte ordering;I am a really really really long string that is over the size of 256 to make sure that endianess is not a problem for byte ordering;I am a really really really long string that is over the size of 256 to make sure that endianess is not a problem for byte ordering".to_string()),
                value: 3.5,
                ts: now_naive + chrono::Duration::hours(2),
                ts_tz: now_tz + chrono::Duration::hours(2),
                inet: None,
                cidr: None,
                sqlx_network: Some(sqlx::types::ipnetwork::IpNetwork::new(std::net::Ipv4Addr::new(192, 168, 255, 255).into(), 24).unwrap()),
            },
        ];

        insert_copy_row_values(&pool, rows.clone(), BufferSize::Fixed(100))
            .await
            .unwrap();

        let fetched = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM test_rows ORDER BY id",
            TestRow::fields()
                .iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .fetch_all(&pool)
        .await
        .unwrap();

        insta::assert_debug_snapshot!(fetched);
    }

    #[tokio::test]
    async fn test_insert_copy_row_values_large_batch() {
        const COUNT: i64 = 5_000;
        let (_container, pool) = start_pg().await;

        let base_tz = chrono::DateTime::parse_from_rfc3339("2024-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows: Vec<TestRow> = (0..COUNT)
            .map(|i| {
                let ts_tz = base_tz + chrono::Duration::seconds(i);
                TestRow {
                    id: i,
                    label: Some(format!("label_{i}")),
                    value: i as f64,
                    ts: ts_tz.naive_utc(),
                    ts_tz,
                    inet: None,
                    cidr: None,
                    sqlx_network: None,
                }
            })
            .collect();

        insert_copy_row_values(&pool, rows, BufferSize::Fixed(100))
            .await
            .unwrap();

        let fetched = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM test_rows ORDER BY id",
            TestRow::fields()
                .iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .fetch_all(&pool)
        .await
        .unwrap();

        insta::assert_debug_snapshot!(fetched);
    }

    #[tokio::test]
    async fn test_insert_copy_row_values_empty() {
        let (_container, pool) = start_pg().await;
        insert_copy_row_values::<TestRow>(&pool, vec![], BufferSize::Fixed(100))
            .await
            .unwrap();
        let fetched = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM test_rows ORDER BY id",
            TestRow::fields()
                .iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .fetch_all(&pool)
        .await
        .unwrap();

        insta::assert_debug_snapshot!(fetched);
    }
}
