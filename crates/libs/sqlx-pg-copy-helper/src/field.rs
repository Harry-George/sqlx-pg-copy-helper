//! Representation of a field to serialize

use tokio_postgres::types::{ToSql, Type};

/// Boxed getter function that extracts a [`FieldValue`] from a row of type `T`.
pub type GetterFunc<T> = Box<dyn for<'a> Fn(&'a T) -> anyhow::Result<FieldValue<'a>> + Send + Sync>;

/// A representation of the value of a field
/// Allows us to return a reference or an owned value
pub enum FieldValue<'a> {
    /// For when we can just return a reference to the value
    Borrowed(&'a dyn ToSql),
    /// For when we need to own the value (e.g. we made a conversion)
    Owned(Box<dyn ToSql + 'a>),
}

impl FieldValue<'_> {
    /// Helper function to convert a reference to a `FieldValue` into a sql value &
    /// return a reference to the value
    #[must_use]
    pub fn as_sql(&self) -> &dyn ToSql {
        match self {
            Self::Borrowed(value) => *value,
            Self::Owned(value) => value.as_ref(),
        }
    }
}

/// A representation of the fields to serialize
pub struct Field<T> {
    /// The `PostgreSQL` type of the field
    pub sql_type: Type,
    /// The name of the field
    pub name: &'static str,
    /// Whether the column may contain `NULL` (i.e. the Rust field is `Option<_>`)
    pub nullable: bool,
    /// A function to get the value of the field from the row
    pub getter_func: GetterFunc<T>,
}
