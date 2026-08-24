# sqlx-pg-copy-helper

Fast bulk inserts into PostgreSQL via the binary `COPY` protocol, driven by a `#[derive(PGCopyTable)]` macro on top of [`sqlx`].

Instead of building `INSERT` statements (or using `UNNEST`/multi-row `VALUES`), this crate streams
rows straight into `COPY ... FROM STDIN WITH (FORMAT BINARY)`, which is the fastest way to load
data into Postgres from an application.

## Example

```rust
use sqlx_pg_copy_helper::{PGCopyTable, BufferSize, insert_copy_row_values};

#[derive(Debug, Clone, PGCopyTable)]
#[pg_copy(table = "events")]
struct Event {
    id: i64,
    label: Option<String>,
    #[pg_copy(name = "ts")]
    ts_tz: chrono::DateTime<chrono::Utc>,
}

async fn insert(pool: &sqlx::PgPool, rows: Vec<Event>) -> anyhow::Result<()> {
    insert_copy_row_values(pool, rows, BufferSize::Default).await
}
```

`#[derive(PGCopyTable)]` inspects each field, infers its `PostgreSQL` column type, and generates
the column list and per-field getters used to serialize the binary COPY stream. Field attributes
(`#[pg_copy(...)]`) let you rename columns, override the inferred SQL type, convert a Rust value
before insertion (`convert` / `try_convert`), skip a field entirely, or flatten a nested struct's
fields inline (`flatten` / `wrapped`).

## Status

Early / pre-1.0 and looking for feedback. The derive macro and public API may still change between minor versions.

## Acknowledgements

The binary COPY encoding is adapted from
[`tokio-postgres`'s `binary_copy` module](https://github.com/rust-postgres/rust-postgres/blob/master/tokio-postgres/src/binary_copy.rs).
We didn't use it directly because it doesn't allow writing into our own buffer, so would not have been compatible with sqlx

## License

MIT — see [LICENSE](https://github.com/Harry-George/sqlx-pg-copy-helper/blob/main/LICENSE).
