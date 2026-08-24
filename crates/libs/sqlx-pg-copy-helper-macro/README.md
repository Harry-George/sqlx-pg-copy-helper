# sqlx-pg-copy-helper-macro

`#[derive(PGCopyTable)]` proc-macro backing [`sqlx-pg-copy-helper`](https://crates.io/crates/sqlx-pg-copy-helper).

This crate is not meant to be used directly — depend on `sqlx-pg-copy-helper`, which re-exports the
derive macro along with the runtime it needs.

Column name and `PostgreSQL` type are inferred from the Rust field name and type.
Both can be overridden with attributes. `IpNetwork` fields are automatically wrapped
with `IpNetworkCidr`; for other types that need conversion supply `convert = "fn_path"`.

All annotations use the single `#[pg_copy(...)]` attribute, matching the sqlx convention.

## Struct-level (exactly one required)

- `#[pg_copy(table = "readings")]` → directly insertable; implements `PgFlattenable` + `PGCopyTable`
- `#[pg_copy(wrapped)]`            → embedded-only; implements only `PgFlattenable`

## Field-level

- `#[pg_copy(name = "col")]`             → override the SQL column name
- `#[pg_copy(sql_type = "FLOAT8")]`      → override the SQL type
- `#[pg_copy(convert = "mod::fn")]`      → infallible conversion `fn(&T) -> U`
- `#[pg_copy(try_convert = "mod::fn")]`  → fallible conversion `fn(&T) -> Result<U>`
- `#[pg_copy(skip)]`                     → exclude this field from all columns
- `#[pg_copy(flatten)]`                  → inline columns of the nested struct

`convert` and `try_convert` are mutually exclusive on a single `#[pg_copy(...)]`.
`skip` and `flatten` are mutually exclusive with each other and with column keys.

Multiple `#[pg_copy(...)]` attributes on one field map it to multiple columns:

```rust,ignore
    #[pg_copy(name = "net_inet", sql_type = "INET")]
    #[pg_copy(name = "net_cidr", sql_type = "CIDR")]
    network: Option<ipnetwork::IpNetwork>,
```

## Example

```rust,ignore
// Embedded key — no table of its own, used only via flatten.
#[derive(PGCopyTable)]
#[pg_copy(wrapped)]
struct ReadingKey {
    device_id: i64,
    sensor: String,
}

// Directly insertable row.
#[derive(PGCopyTable)]
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
    #[pg_copy(convert = "my_mod::to_pg_value")]
    custom: MyType,
    #[pg_copy(try_convert = "my_mod::try_to_pg_value")]
    other: OtherType,
    #[pg_copy(skip)]
    internal_flag: bool,
    #[pg_copy(flatten)]
    key: ReadingKey,
}
```

## License

MIT — see [LICENSE](https://github.com/Harry-George/sqlx-pg-copy-helper/blob/main/LICENSE).
