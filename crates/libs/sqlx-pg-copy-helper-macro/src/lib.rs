#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Field as SynField, Fields, Meta, Path, Type, parse_macro_input};

/// Parsed representation of column-mapping keys within one `#[pg_copy(...)]` annotation.
struct PgColumnAttr {
    /// Override for the SQL column name; if `None`, the field ident is used.
    name: Option<String>,
    /// The `tokio_postgres::types::Type` constant name, e.g. `"INT8"`.
    /// If `None`, the type is inferred from the Rust field type.
    sql_type: Option<String>,
    /// Infallible conversion function called as `fn(&value) -> T`.
    /// Mutually exclusive with `try_convert`.
    convert: Option<Path>,
    /// Fallible conversion function called as `fn(&value) -> sqlx_pg_copy_helper::Result<T>`.
    /// Mutually exclusive with `convert`.
    try_convert: Option<Path>,
}

/// The semantic meaning of one `#[pg_copy(...)]` on a field.
enum PgFieldAttrKind {
    /// Map this field to one or more columns using the provided column spec.
    Column(PgColumnAttr),
    /// Exclude this field from all columns.
    Skip,
    /// Inline the nested struct's columns at this position.
    Flatten,
}

/// All annotations collected from one struct field.
struct FieldInfo<'a> {
    /// The Rust field identifier.
    ident: &'a syn::Ident,
    /// The Rust field type (used for inference and to detect `Option<_>`).
    ty: &'a Type,
    /// One entry per `#[pg_copy(...)]` that maps to a column.
    columns: Vec<PgColumnAttr>,
    /// Whether `#[pg_copy(skip)]` was present.
    skip: bool,
    /// Whether `#[pg_copy(flatten)]` was present.
    flatten: bool,
}

/// Whether the struct is directly insertable or only usable as an embedded type.
enum StructMode {
    /// `#[pg_copy(table = "name")]`
    Table(String),
    /// `#[pg_copy(wrapped)]`
    Wrapped,
}

/// Returns the inner type of `Option<T>`, or the type itself if not `Option`.
fn unwrap_option(ty: &Type) -> &Type {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return inner;
    }
    ty
}

/// Returns `true` if the base type (stripping any `Option`) is `IpNetwork`.
fn is_ip_network_type(ty: &Type) -> bool {
    if let Type::Path(tp) = unwrap_option(ty)
        && let Some(seg) = tp.path.segments.last()
    {
        return seg.ident == "IpNetwork";
    }
    false
}

/// Returns `true` if the outermost type is `Option<_>`.
fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return segment.ident == "Option";
    }
    false
}

/// Infers the `PostgreSQL` type name from a Rust type, recursing into `Option<T>`.
/// Returns `None` for types that cannot be automatically mapped.
fn infer_sql_type(ty: &Type) -> Option<&'static str> {
    let Type::Path(tp) = ty else { return None };
    let segment = tp.path.segments.last()?;
    match segment.ident.to_string().as_str() {
        "i16" => Some("INT2"),
        "i32" => Some("INT4"),
        "i64" => Some("INT8"),
        // Floats - note we always use the biggest type possible to avoid overflows.
        "f32" | "f64" => Some("FLOAT8"),
        // Primitives
        "bool" => Some("BOOL"),
        "String" => Some("VARCHAR"),
        // Dates / times  (chrono)
        "NaiveDateTime" => Some("TIMESTAMP"),
        "DateTime" => Some("TIMESTAMPTZ"),
        "NaiveDate" => Some("DATE"),
        "NaiveTime" => Some("TIME"),
        // Network
        "IpAddr" => Some("INET"),
        // IpCidr and IpNetwork both default to CIDR; IpNetwork can be overridden to INET via sql_type = "INET"
        "IpCidr" | "IpNetwork" => Some("CIDR"),
        // UUID
        "Uuid" => Some("UUID"),
        // JSON
        "Value" => Some("JSONB"),
        // Unwrap Option<T> and infer from the inner type
        "Option" => {
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
            {
                return infer_sql_type(inner);
            }
            None
        }
        _ => None,
    }
}

/// Maps a SQL type name string to a `tokio_postgres::types::Type` constant token stream.
fn sql_type_to_tokens(sql_type: &str, span: proc_macro2::Span) -> Result<TokenStream2, syn::Error> {
    const SUPPORTED_SQL_TYPES: &[&str] = &[
        "INT2",
        "INT4",
        "INT8",
        "FLOAT4",
        "FLOAT8",
        "TEXT",
        "VARCHAR",
        "BPCHAR",
        "CHAR",
        "BOOL",
        "BYTEA",
        "TIMESTAMP",
        "TIMESTAMPTZ",
        "DATE",
        "TIME",
        "TIMETZ",
        "UUID",
        "JSON",
        "JSONB",
        "INET",
        "CIDR",
        "NUMERIC",
        "OID",
    ];

    if SUPPORTED_SQL_TYPES.contains(&sql_type) {
        let ident = syn::Ident::new(sql_type, span);
        Ok(quote! { ::tokio_postgres::types::Type::#ident })
    } else {
        Err(syn::Error::new(
            span,
            format!(
                "unknown sql_type \"{sql_type}\"; supported: {}",
                SUPPORTED_SQL_TYPES.join(", ")
            ),
        ))
    }
}

/// Parses one `#[pg_copy(...)]` on a field into its semantic kind.
///
/// A bare `#[pg_copy]` or empty `#[pg_copy()]` is treated as a column with all defaults,
/// which is useful to map a field to a second column alongside an explicit one.
fn parse_pg_field_attr(attr: &syn::Attribute) -> Result<PgFieldAttrKind, syn::Error> {
    // Bare #[pg_copy] with no argument list → column with all defaults.
    if let Meta::Path(_) = &attr.meta {
        return Ok(PgFieldAttrKind::Column(PgColumnAttr {
            name: None,
            sql_type: None,
            convert: None,
            try_convert: None,
        }));
    }

    let mut col_name: Option<String> = None;
    let mut sql_type: Option<String> = None;
    let mut convert: Option<Path> = None;
    let mut try_convert: Option<Path> = None;
    let mut skip = false;
    let mut flatten = false;

    attr.meta.require_list()?.parse_nested_meta(|meta| {
        if meta.path.is_ident("skip") {
            skip = true;
        } else if meta.path.is_ident("flatten") {
            flatten = true;
        } else if meta.path.is_ident("name") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            col_name = Some(lit.value());
        } else if meta.path.is_ident("sql_type") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            sql_type = Some(lit.value());
        } else if meta.path.is_ident("convert") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            convert = Some(syn::parse_str::<Path>(&lit.value())?);
        } else if meta.path.is_ident("try_convert") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            try_convert = Some(syn::parse_str::<Path>(&lit.value())?);
        } else {
            return Err(meta.error(
                "unknown pg_copy key; expected `name`, `sql_type`, `convert`, \
                 `try_convert`, `skip`, or `flatten`",
            ));
        }
        Ok(())
    })?;

    let has_col_keys =
        col_name.is_some() || sql_type.is_some() || convert.is_some() || try_convert.is_some();

    if skip && flatten {
        return Err(syn::Error::new_spanned(
            attr,
            "`skip` and `flatten` are mutually exclusive",
        ));
    }
    if skip && has_col_keys {
        return Err(syn::Error::new_spanned(
            attr,
            "`skip` cannot be combined with column keys (`name`, `sql_type`, `convert`, `try_convert`)",
        ));
    }
    if flatten && has_col_keys {
        return Err(syn::Error::new_spanned(
            attr,
            "`flatten` cannot be combined with column keys (`name`, `sql_type`, `convert`, `try_convert`)",
        ));
    }
    if convert.is_some() && try_convert.is_some() {
        return Err(syn::Error::new_spanned(
            attr,
            "`convert` and `try_convert` are mutually exclusive",
        ));
    }

    if skip {
        return Ok(PgFieldAttrKind::Skip);
    }
    if flatten {
        return Ok(PgFieldAttrKind::Flatten);
    }
    Ok(PgFieldAttrKind::Column(PgColumnAttr {
        name: col_name,
        sql_type,
        convert,
        try_convert,
    }))
}

/// Parses the single `#[pg_copy(...)]` on the struct into a [`StructMode`].
fn parse_pg_struct_attr(attr: &syn::Attribute) -> Result<StructMode, syn::Error> {
    let mut table_name: Option<String> = None;
    let mut wrapped = false;

    attr.meta.require_list()?.parse_nested_meta(|meta| {
        if meta.path.is_ident("table") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            table_name = Some(lit.value());
        } else if meta.path.is_ident("wrapped") {
            wrapped = true;
        } else {
            return Err(
                meta.error("unknown pg_copy struct key; expected `table = \"name\"` or `wrapped`")
            );
        }
        Ok(())
    })?;

    match (table_name, wrapped) {
        (Some(_), true) => Err(syn::Error::new_spanned(
            attr,
            "`table` and `wrapped` are mutually exclusive inside `#[pg_copy(...)]`",
        )),
        (None, false) => Err(syn::Error::new_spanned(
            attr,
            "`#[pg_copy(...)]` on a struct requires either `table = \"name\"` or `wrapped`",
        )),
        (Some(name), false) => Ok(StructMode::Table(name)),
        (None, true) => Ok(StructMode::Wrapped),
    }
}

/// Finds the single `#[pg_copy(...)]` on the struct and parses it.
fn extract_struct_mode(attrs: &[syn::Attribute]) -> Result<StructMode, syn::Error> {
    let pg_attrs: Vec<_> = attrs
        .iter()
        .filter(|a| a.path().is_ident("pg_copy"))
        .collect();

    match pg_attrs.as_slice() {
        [] => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "PGCopyTable derive requires `#[pg_copy(table = \"name\")]` or \
             `#[pg_copy(wrapped)]` on the struct",
        )),
        [single] => parse_pg_struct_attr(single),
        [first, ..] => Err(syn::Error::new_spanned(
            first,
            "only one `#[pg_copy(...)]` attribute is allowed on the struct",
        )),
    }
}

/// Collects all `#[pg_copy(...)]` annotations from a struct field.
fn collect_field_info(field: &SynField) -> Result<FieldInfo<'_>, syn::Error> {
    let ident = field.ident.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(field, "PGCopyTable only supports named struct fields")
    })?;

    let mut columns: Vec<PgColumnAttr> = Vec::new();
    let mut skip = false;
    let mut flatten = false;

    for attr in &field.attrs {
        if !attr.path().is_ident("pg_copy") {
            continue;
        }
        match parse_pg_field_attr(attr)? {
            PgFieldAttrKind::Column(col) => columns.push(col),
            PgFieldAttrKind::Skip => skip = true,
            PgFieldAttrKind::Flatten => flatten = true,
        }
    }

    if flatten && (!columns.is_empty() || skip) {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[pg_copy(flatten)]` is mutually exclusive with column attributes and `skip`",
        ));
    }

    Ok(FieldInfo {
        ident,
        ty: &field.ty,
        columns,
        skip,
        flatten,
    })
}

/// Generates the `Field { ... }` token stream for a single column spec.
///
/// Conversion priority:
/// 1. Explicit `convert = "fn_path"` → call `fn_path(&value)` (infallible, returns `T`)
/// 2. Explicit `try_convert = "fn_path"` → call `fn_path(&value)` (fallible, returns `Result<T>`)
/// 3. Field type is `IpNetwork` (or `Option<IpNetwork>`) → auto-wrap with `IpNetworkCidr`
/// 4. Otherwise → borrow directly
fn generate_field_tokens(
    field_ident: &syn::Ident,
    field_ty: &Type,
    is_option: bool,
    col: &PgColumnAttr,
) -> Result<TokenStream2, syn::Error> {
    let col_name_str = col
        .name
        .as_deref()
        .unwrap_or(&field_ident.to_string())
        .to_owned();

    let sql_type_str = match &col.sql_type {
        Some(s) => s.clone(),
        None => infer_sql_type(field_ty)
            .ok_or_else(|| {
                syn::Error::new_spanned(
                    field_ident,
                    "cannot infer PostgreSQL type for this Rust type; \
                     add `sql_type = \"...\"` to `#[pg_copy(...)]`",
                )
            })?
            .to_owned(),
    };

    let type_tokens = sql_type_to_tokens(&sql_type_str, proc_macro2::Span::call_site())?;

    let getter_func = if let Some(convert_path) = &col.convert {
        // conversion that returns a value: fn(&T) -> U
        if is_option {
            quote! {
                ::std::boxed::Box::new(|r| Ok(::sqlx_pg_copy_helper::FieldValue::Owned(
                    ::std::boxed::Box::new(r.#field_ident.as_ref().map(#convert_path))
                )))
            }
        } else {
            quote! {
                ::std::boxed::Box::new(|r| Ok(::sqlx_pg_copy_helper::FieldValue::Owned(
                    ::std::boxed::Box::new(#convert_path(&r.#field_ident))
                )))
            }
        }
    } else if let Some(try_convert_path) = &col.try_convert {
        // Conversion that returns a result: fn(&T) -> sqlx_pg_copy_helper::Result<U>
        if is_option {
            quote! {
                ::std::boxed::Box::new(|r| r.#field_ident.as_ref().map(#try_convert_path).transpose().map(|opt|
                    ::sqlx_pg_copy_helper::FieldValue::Owned(::std::boxed::Box::new(opt))
                ))
            }
        } else {
            quote! {
                ::std::boxed::Box::new(|r| {
                    let result = #try_convert_path(&r.#field_ident);
                    match result {
                        Ok(v) => Ok(::sqlx_pg_copy_helper::FieldValue::Owned(::std::boxed::Box::new(v))),
                        Err(e) => Err(e),
                    }
                })
            }
        }
    } else if is_ip_network_type(field_ty) {
        // IpNetwork auto-conversion: wrap with IpNetworkCidr (handles both INET and CIDR).
        if is_option {
            quote! {
                ::std::boxed::Box::new(|r| Ok(::sqlx_pg_copy_helper::FieldValue::Owned(
                    ::std::boxed::Box::new(r.#field_ident.map(::sqlx_pg_copy_helper::IpNetworkCidr))
                )))
            }
        } else {
            quote! {
                ::std::boxed::Box::new(|r| Ok(::sqlx_pg_copy_helper::FieldValue::Owned(
                    ::std::boxed::Box::new(::sqlx_pg_copy_helper::IpNetworkCidr(r.#field_ident))
                )))
            }
        }
    } else {
        // Just a straight pass through, so just wrap the field
        quote! {
            ::std::boxed::Box::new(|r| Ok(::sqlx_pg_copy_helper::FieldValue::Borrowed(&r.#field_ident)))
        }
    };

    Ok(quote! {
        ::sqlx_pg_copy_helper::Field {
            sql_type: #type_tokens,
            name: #col_name_str,
            nullable: #is_option,
            getter_func: #getter_func,
        }
    })
}

/// Entry point for the `#[derive(PGCopyTable)]` proc-macro.
///
/// Generates `PgFlattenable` (and optionally `PGCopyTable`) for the annotated struct.
#[proc_macro_derive(PGCopyTable, attributes(pg_copy))]
pub fn derive_binary_copy_insert(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    match derive_impl(&ast) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Inner implementation, returning `Result` for clean error propagation.
fn derive_impl(ast: &DeriveInput) -> Result<TokenStream2, syn::Error> {
    let struct_ident = &ast.ident;
    let mode = extract_struct_mode(&ast.attrs)?;

    let fields = match &ast.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    &ast.ident,
                    "PGCopyTable can only be derived for structs with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                &ast.ident,
                "PGCopyTable can only be derived for structs",
            ));
        }
    };

    let mut field_statements: Vec<TokenStream2> = Vec::new();

    for syn_field in fields {
        let info = collect_field_info(syn_field)?;

        if info.skip {
            continue;
        }

        if info.flatten {
            let field_ident = info.ident;
            let field_ty = info.ty;
            // At runtime, call `<NestedType>::fields()` to get all of the nested struct's
            // `Field<NestedType>` descriptors, then re-wrap each one as a `Field<OuterType>`.
            //
            // each nested field has a getter
            //   `fn(&NestedType) -> Result<FieldValue>`
            // but we need
            //   `fn(&OuterType) -> Result<FieldValue>`
            //
            // Move the nested getter into a new closure that first navigates from the outer
            // struct to the nested field
            // (`&r.field_ident`),
            // then delegates to the original getter.
            field_statements.push(quote! {
                __fields.extend(
                    <#field_ty as ::sqlx_pg_copy_helper::PgFlattenable>::fields()
                        .into_iter()
                        .map(|__nested_field| {
                            let __nested_getter = __nested_field.getter_func;
                            ::sqlx_pg_copy_helper::Field {
                                sql_type: __nested_field.sql_type,
                                name: __nested_field.name,
                                nullable: __nested_field.nullable,
                                getter_func: ::std::boxed::Box::new(
                                    move |r: &#struct_ident| __nested_getter(&r.#field_ident)
                                ),
                            }
                        })
                );
            });
            continue;
        }

        let is_option = is_option_type(info.ty);

        if info.columns.is_empty() {
            let default_col = PgColumnAttr {
                name: None,
                sql_type: None,
                convert: None,
                try_convert: None,
            };
            let field_tokens = generate_field_tokens(info.ident, info.ty, is_option, &default_col)?;
            field_statements.push(quote! { __fields.push(#field_tokens); });
        } else {
            for col in &info.columns {
                let field_tokens = generate_field_tokens(info.ident, info.ty, is_option, col)?;
                field_statements.push(quote! { __fields.push(#field_tokens); });
            }
        }
    }

    let flattenable_impl = quote! {
        impl ::sqlx_pg_copy_helper::PgFlattenable for #struct_ident {
            fn fields() -> ::std::vec::Vec<::sqlx_pg_copy_helper::Field<Self>> {
                let mut __fields: ::std::vec::Vec<::sqlx_pg_copy_helper::Field<#struct_ident>> =
                    ::std::vec::Vec::new();
                #(#field_statements)*
                __fields
            }
        }
    };

    let binary_copy_impl = match mode {
        StructMode::Table(table_name) => quote! {
            impl ::sqlx_pg_copy_helper::PGCopyTable for #struct_ident {
                fn table_name() -> &'static str {
                    #table_name
                }
            }
        },
        StructMode::Wrapped => quote! {},
    };

    Ok(quote! {
        #flattenable_impl
        #binary_copy_impl
    })
}
