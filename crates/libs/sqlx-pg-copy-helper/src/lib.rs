#![doc = include_str!("../README.md")]

mod binary_copy_insert;
mod field;
mod insert_copy_row_values;
mod ip_network_cidr;

pub use anyhow::Result;
pub use binary_copy_insert::{PGCopyTable, PgFlattenable};
pub use field::{Field, FieldValue, GetterFunc};
pub use insert_copy_row_values::{BufferSize, generate_create_table_string, insert_copy_row_values};
pub use ip_network_cidr::IpNetworkCidr;
pub use sqlx_pg_copy_helper_macro::PGCopyTable;
