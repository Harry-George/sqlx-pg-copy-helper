//! Traits for structs that participate in the `PostgreSQL` binary COPY protocol.

use crate::Field;

/// Provides the column definitions for a struct.
///
/// Implemented by both directly-insertable structs (`#[pg_table]`) and structs that are only
/// ever embedded via `#[pg_flatten]` (`#[pg_wrapped]`).
pub trait PgFlattenable: Sized {
    fn fields() -> Vec<Field<Self>>;
}

/// Extends [`PgFlattenable`] with a table name, enabling direct insertion via
/// `insert_copy_row_values`.  Derived by structs annotated with `#[pg_table = "..."]`.
pub trait PGCopyTable: PgFlattenable {
    fn table_name() -> &'static str;
}
