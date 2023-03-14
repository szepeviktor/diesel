use crate::config;
use crate::database::{Backend, InferConnection};
use crate::infer_schema_internals::*;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_regex::Serde as RegexWrapper;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter, Write};
use std::io::Write as IoWrite;

const SCHEMA_HEADER: &str = "// @generated automatically by Diesel CLI.\n";

type Regex = RegexWrapper<::regex::Regex>;

pub enum Filtering {
    OnlyTables(Vec<Regex>),
    ExceptTables(Vec<Regex>),
    None,
}

#[allow(clippy::derivable_impls)] // that's not supported on rust 1.65
impl Default for Filtering {
    fn default() -> Self {
        Filtering::None
    }
}

impl Filtering {
    pub fn should_ignore_table(&self, name: &TableName) -> bool {
        use self::Filtering::*;

        match *self {
            OnlyTables(ref regexes) => !regexes.iter().any(|regex| regex.is_match(&name.sql_name)),
            ExceptTables(ref regexes) => regexes.iter().any(|regex| regex.is_match(&name.sql_name)),
            None => false,
        }
    }
}

/// How to sort columns when querying the table schema.
#[derive(Debug, Deserialize, Serialize)]
pub enum ColumnSorting {
    /// Order by ordinal position
    #[serde(rename = "ordinal_position")]
    OrdinalPosition,
    /// Order by column name
    #[serde(rename = "name")]
    Name,
}

#[allow(clippy::derivable_impls)] // that's not supported on rust 1.65
impl Default for ColumnSorting {
    fn default() -> Self {
        ColumnSorting::OrdinalPosition
    }
}

#[derive(Clone, Copy)]
pub enum DocConfig {
    DatabaseCommentsFallbackToAutoGeneratedDocComment,
    OnlyDatabaseComments,
    NoDocComments,
}

#[allow(clippy::derivable_impls)] // that's not supported on rust 1.65
impl Default for DocConfig {
    fn default() -> Self {
        DocConfig::NoDocComments
    }
}

pub fn run_print_schema<W: IoWrite>(
    connection: &mut InferConnection,
    config: &config::PrintSchema,
    output: &mut W,
) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    let schema = output_schema(connection, config)?;

    output.write_all(schema.as_bytes())?;
    Ok(())
}

fn common_diesel_types(types: &mut HashSet<&str>) {
    types.insert("Bool");
    types.insert("Integer");
    types.insert("SmallInt");
    types.insert("BigInt");
    types.insert("Binary");
    types.insert("Text");
    types.insert("Double");
    types.insert("Float");
    types.insert("Numeric");
    types.insert("Timestamp");
    types.insert("Date");
    types.insert("Time");

    // hidden type defs
    types.insert("Float4");
    types.insert("Smallint");
    types.insert("Int2");
    types.insert("Int4");
    types.insert("Int8");
    types.insert("Bigint");
    types.insert("Float8");
    types.insert("Decimal");
    types.insert("VarChar");
    types.insert("Varchar");
    types.insert("Char");
    types.insert("Tinytext");
    types.insert("Mediumtext");
    types.insert("Longtext");
    types.insert("Tinyblob");
    types.insert("Blob");
    types.insert("Mediumblob");
    types.insert("Longblob");
    types.insert("Varbinary");
    types.insert("Bit");
}

#[cfg(feature = "postgres")]
fn pg_diesel_types() -> HashSet<&'static str> {
    let mut types = HashSet::new();
    types.insert("Cidr");
    types.insert("Inet");
    types.insert("Jsonb");
    types.insert("MacAddr");
    types.insert("Money");
    types.insert("Oid");
    types.insert("Range");
    types.insert("Timestamptz");
    types.insert("Uuid");
    types.insert("Json");
    types.insert("Record");
    types.insert("Interval");

    // hidden type defs
    types.insert("Int4range");
    types.insert("Int8range");
    types.insert("Daterange");
    types.insert("Numrange");
    types.insert("Tsrange");
    types.insert("Tstzrange");
    types.insert("SmallSerial");
    types.insert("BigSerial");
    types.insert("Serial");
    types.insert("Bytea");
    types.insert("Bpchar");
    types.insert("Macaddr");

    common_diesel_types(&mut types);
    types
}

#[cfg(feature = "mysql")]
fn mysql_diesel_types() -> HashSet<&'static str> {
    let mut types = HashSet::new();
    common_diesel_types(&mut types);

    types.insert("TinyInt");
    types.insert("Tinyint");
    types.insert("Datetime");
    types.insert("Json");
    types
}

#[cfg(feature = "sqlite")]
fn sqlite_diesel_types() -> HashSet<&'static str> {
    let mut types = HashSet::new();
    common_diesel_types(&mut types);
    types
}

pub fn output_schema(
    connection: &mut InferConnection,
    config: &config::PrintSchema,
) -> Result<String, Box<dyn Error + Send + Sync + 'static>> {
    let table_names = load_table_names(connection, config.schema_name())?
        .into_iter()
        .filter(|t| !config.filter.should_ignore_table(t))
        .collect::<Vec<_>>();
    let foreign_keys = load_foreign_key_constraints(connection, config.schema_name())?;
    let foreign_keys =
        remove_unsafe_foreign_keys_for_codegen(connection, &foreign_keys, &table_names);
    let table_data = table_names
        .into_iter()
        .map(|t| load_table_data(connection, t, &config.column_sorting, config.with_docs))
        .collect::<Result<Vec<_>, Box<dyn Error + Send + Sync + 'static>>>()?;

    let mut out = String::new();
    writeln!(out, "{SCHEMA_HEADER}")?;

    let backend = Backend::for_connection(connection);

    let columns_custom_types = if config.generate_missing_sql_type_definitions() {
        let diesel_provided_types = match backend {
            #[cfg(feature = "postgres")]
            Backend::Pg => pg_diesel_types(),
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => sqlite_diesel_types(),
            #[cfg(feature = "mysql")]
            Backend::Mysql => mysql_diesel_types(),
        };

        Some(
            table_data
                .iter()
                .map(|t| {
                    t.column_data
                        .iter()
                        .map(|c| {
                            Some(&c.ty)
                                .filter(|ty| !diesel_provided_types.contains(ty.rust_name.as_str()))
                                .map(|ty| match backend {
                                    #[cfg(feature = "postgres")]
                                    Backend::Pg => ty.clone(),
                                    #[cfg(feature = "sqlite")]
                                    Backend::Sqlite => ty.clone(),
                                    #[cfg(feature = "mysql")]
                                    Backend::Mysql => {
                                        // For MySQL we generate custom types for unknown types that
                                        // are dedicated to the column
                                        use heck::ToUpperCamelCase;

                                        ColumnType {
                                            rust_name: format!(
                                                "{} {} {}",
                                                &t.name.rust_name, &c.rust_name, &ty.rust_name
                                            )
                                            .to_upper_camel_case(),
                                            ..ty.clone()
                                        }
                                    }
                                })
                        })
                        .collect::<Vec<Option<ColumnType>>>()
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };

    let definitions = TableDefinitions {
        tables: table_data,
        fk_constraints: foreign_keys,
        with_docs: config.with_docs,
        custom_types_for_tables: columns_custom_types.map(|custom_types_sorted| {
            CustomTypesForTables {
                backend,
                types_overrides_sorted: custom_types_sorted,
                with_docs: match config.with_docs {
                    DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment => true,
                    DocConfig::OnlyDatabaseComments | DocConfig::NoDocComments => false,
                },
                #[cfg(any(feature = "postgres", feature = "mysql"))]
                derives: config.custom_type_derives(),
            }
        }),
        import_types: config.import_types(),
    };

    if let Some(schema_name) = config.schema_name() {
        write!(out, "{}", ModuleDefinition(schema_name, definitions))?;
    } else {
        if let Some(ref custom_types_for_tables) = definitions.custom_types_for_tables {
            write!(
                out,
                "{}",
                CustomTypesForTablesForDisplay {
                    custom_types: custom_types_for_tables,
                    tables: &definitions.tables
                }
            )?;
        }

        write!(out, "{definitions}")?;
    }

    if let Some(ref patch_file) = config.patch_file {
        let patch = std::fs::read_to_string(patch_file)?;
        let patch = diffy::Patch::from_str(&patch)?;

        out = diffy::apply(&out, &patch)?;
    }

    Ok(out)
}

struct CustomTypesForTables {
    backend: Backend,
    // To be zipped with tables then columns
    types_overrides_sorted: Vec<Vec<Option<ColumnType>>>,
    with_docs: bool,
    #[cfg(any(feature = "postgres", feature = "mysql"))]
    derives: Vec<String>,
}

pub struct CustomTypesForTablesForDisplay<'a> {
    custom_types: &'a CustomTypesForTables,
    tables: &'a [TableData],
}

#[allow(clippy::print_in_format_impl)]
impl Display for CustomTypesForTablesForDisplay<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.custom_types.backend {
            #[cfg(feature = "postgres")]
            Backend::Pg => {
                let _ = &self.tables;
                let mut types_to_generate: Vec<&ColumnType> = self
                    .custom_types
                    .types_overrides_sorted
                    .iter()
                    .flatten()
                    .flatten()
                    .collect();
                if types_to_generate.is_empty() {
                    return Ok(());
                }
                types_to_generate
                    .sort_unstable_by_key(|column_type| column_type.rust_name.as_str());
                // On PG we expect that there may be duplicates because types names are not made
                // specific to the column, unlike MySQL
                types_to_generate.dedup_by_key(|column_type| column_type.rust_name.as_str());

                if self.custom_types.with_docs {
                    writeln!(f, "/// A module containing custom SQL type definitions")?;
                    writeln!(f, "///")?;
                    writeln!(f, "/// (Automatically generated by Diesel.)")?;
                }
                let mut out = PadAdapter::new(f);
                writeln!(out, "pub mod sql_types {{")?;

                for (idx, &ct) in types_to_generate.iter().enumerate() {
                    if idx != 0 {
                        writeln!(out)?;
                    }
                    if self.custom_types.with_docs {
                        if let Some(ref schema) = ct.schema {
                            writeln!(out, "/// The `{}.{}` SQL type", schema, ct.sql_name)?;
                        } else {
                            writeln!(out, "/// The `{}` SQL type", ct.sql_name)?;
                        }
                        writeln!(out, "///")?;
                        writeln!(out, "/// (Automatically generated by Diesel.)")?;
                    }
                    writeln!(out, "#[derive({})]", self.custom_types.derives.join(", "))?;
                    if let Some(ref schema) = ct.schema {
                        writeln!(
                            out,
                            "#[diesel(postgres_type(name = \"{}\", schema = \"{}\"))]",
                            ct.sql_name, schema
                        )?;
                    } else {
                        writeln!(out, "#[diesel(postgres_type(name = \"{}\"))]", ct.sql_name)?;
                    }
                    writeln!(out, "pub struct {};", ct.rust_name)?;
                }

                writeln!(f, "}}\n")?;
                Ok(())
            }
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => {
                let _ = (&f, self.custom_types.with_docs, &self.tables);

                let mut types_to_generate: Vec<&ColumnType> = self
                    .custom_types
                    .types_overrides_sorted
                    .iter()
                    .flatten()
                    .flatten()
                    .collect();
                types_to_generate
                    .sort_unstable_by_key(|column_type| column_type.rust_name.as_str());

                if types_to_generate.is_empty() {
                    return Ok(());
                }
                for t in &types_to_generate {
                    eprintln!("Encountered unknown type for Sqlite: {}", t.sql_name);
                }
                unreachable!(
                    "Diesel only support a closed set of types for Sqlite. \
                     If you ever see this error message please open an \
                     issue at https://github.com/diesel-rs/diesel containing \
                     a dump of your schema definition."
                )
            }
            #[cfg(feature = "mysql")]
            Backend::Mysql => {
                let mut types_to_generate: Vec<(&ColumnType, &TableName, &ColumnDefinition)> = self
                    .custom_types
                    .types_overrides_sorted
                    .iter()
                    .zip(self.tables)
                    .flat_map(|(ct, t)| {
                        ct.iter()
                            .zip(&t.column_data)
                            .map(move |(ct, c)| (ct, c, &t.name))
                    })
                    .filter_map(|(ct, c, t)| ct.as_ref().map(|ct| (ct, t, c)))
                    .collect();
                if types_to_generate.is_empty() {
                    return Ok(());
                }
                types_to_generate.sort_by_key(|(column_type, _, _)| column_type.rust_name.as_str());

                if self.custom_types.with_docs {
                    writeln!(f, "/// A module containing custom SQL type definitions")?;
                    writeln!(f, "///")?;
                    writeln!(f, "/// (Automatically generated by Diesel.)")?;
                }

                let mut out = PadAdapter::new(f);
                writeln!(out, "pub mod sql_types {{")?;

                for (idx, &(custom_type, table, column)) in types_to_generate.iter().enumerate() {
                    if idx != 0 {
                        writeln!(out)?;
                    }

                    if self.custom_types.with_docs {
                        writeln!(
                            out,
                            "/// The `{}` SQL type for the\n\
                             /// [`{tbl}::{col}`](super::{tbl}::{col})) column",
                            custom_type.sql_name,
                            tbl = table.rust_name,
                            col = column.rust_name,
                        )?;
                        writeln!(out, "///")?;
                        writeln!(out, "/// (Automatically generated by Diesel.)")?;
                    }

                    writeln!(out, "#[derive({})]", self.custom_types.derives.join(", "))?;

                    let mysql_name = {
                        let mut c = custom_type.sql_name.chars();

                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                        }
                    };

                    writeln!(out, "#[diesel(mysql_type(name = \"{mysql_name}\"))]")?;
                    writeln!(out, "pub struct {};", custom_type.rust_name)?;
                }

                writeln!(f, "}}\n")?;
                Ok(())
            }
        }
    }
}

struct ModuleDefinition<'a>(&'a str, TableDefinitions<'a>);

impl<'a> Display for ModuleDefinition<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        {
            let mut out = PadAdapter::new(f);
            writeln!(out, "pub mod {} {{", self.0)?;
            if let Some(ref custom_types_for_tables) = self.1.custom_types_for_tables {
                write!(
                    out,
                    "{}",
                    CustomTypesForTablesForDisplay {
                        custom_types: custom_types_for_tables,
                        tables: &self.1.tables
                    }
                )?;
            }
            write!(out, "{}", self.1)?;
        }
        writeln!(f, "}}")?;
        Ok(())
    }
}

struct TableDefinitions<'a> {
    tables: Vec<TableData>,
    fk_constraints: Vec<ForeignKeyConstraint>,
    with_docs: DocConfig,
    import_types: Option<&'a [String]>,
    custom_types_for_tables: Option<CustomTypesForTables>,
}

impl<'a> Display for TableDefinitions<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let mut is_first = true;
        for (table_idx, table) in self.tables.iter().enumerate() {
            if is_first {
                is_first = false;
            } else {
                writeln!(f)?;
            }
            writeln!(
                f,
                "{}",
                TableDefinition {
                    table,
                    with_docs: self.with_docs,
                    import_types: self.import_types,
                    custom_type_overrides: self
                        .custom_types_for_tables
                        .as_ref()
                        .map(|cts| cts.types_overrides_sorted[table_idx].as_slice())
                }
            )?;
        }

        if !self.fk_constraints.is_empty() {
            writeln!(f)?;
        }

        for foreign_key in &self.fk_constraints {
            writeln!(f, "{}", Joinable(foreign_key))?;
        }

        if self.tables.len() > 1 {
            write!(f, "\ndiesel::allow_tables_to_appear_in_same_query!(")?;
            {
                let mut out = PadAdapter::new(f);
                writeln!(out)?;
                for table in &self.tables {
                    if table.name.rust_name == table.name.sql_name {
                        writeln!(out, "{},", table.name.sql_name)?;
                    } else {
                        writeln!(out, "{},", table.name.rust_name)?;
                    }
                }
            }
            writeln!(f, ");")?;
        }

        Ok(())
    }
}

struct TableDefinition<'a> {
    table: &'a TableData,
    with_docs: DocConfig,
    import_types: Option<&'a [String]>,
    custom_type_overrides: Option<&'a [Option<ColumnType>]>,
}

fn write_doc_comments(out: &mut impl fmt::Write, doc: &str) -> fmt::Result {
    for line in doc.lines() {
        let line = line.trim();
        writeln!(out, "///{}{}", if line.is_empty() { "" } else { " " }, line)?;
    }
    Ok(())
}

impl<'a> Display for TableDefinition<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "diesel::table! {{")?;
        {
            let mut out = PadAdapter::new(f);
            writeln!(out)?;

            let mut has_written_import = false;
            if let Some(types) = self.import_types {
                for import in types {
                    writeln!(out, "use {import};")?;
                    has_written_import = true;
                }
            }

            #[cfg(any(feature = "mysql", feature = "postgres"))]
            {
                let mut already_imported_custom_types: HashSet<&str> = HashSet::new();
                for ct in self
                    .custom_type_overrides
                    .iter()
                    .copied()
                    .flatten()
                    .filter_map(|opt| opt.as_ref())
                {
                    if already_imported_custom_types.insert(&ct.rust_name) {
                        if !has_written_import {
                            writeln!(out, "use diesel::sql_types::*;")?;
                        }
                        writeln!(out, "use super::sql_types::{};", ct.rust_name)?;
                        has_written_import = true;
                    }
                }
            }

            #[cfg(not(any(feature = "mysql", feature = "postgres")))]
            let _ = self.custom_type_overrides;

            if has_written_import {
                writeln!(out)?;
            }

            let full_sql_name = self.table.name.full_sql_name();

            match self.with_docs {
                DocConfig::NoDocComments => {}
                DocConfig::OnlyDatabaseComments => {
                    if let Some(comment) = self.table.comment.as_deref() {
                        write_doc_comments(&mut out, comment)?;
                    }
                }
                DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment => {
                    if let Some(comment) = self.table.comment.as_deref() {
                        write_doc_comments(&mut out, comment)?;
                    } else {
                        write_doc_comments(
                            &mut out,
                            &format!(
                                "Representation of the `{full_sql_name}` table.

                                (Automatically generated by Diesel.)",
                            ),
                        )?;
                    }
                }
            }

            if self.table.name.rust_name != self.table.name.sql_name {
                writeln!(out, r#"#[sql_name = "{full_sql_name}"]"#,)?;
            }

            write!(out, "{} (", self.table.name)?;

            for (i, pk) in self.table.primary_key.iter().enumerate() {
                if i != 0 {
                    write!(out, ", ")?;
                }
                write!(out, "{pk}")?;
            }

            write!(
                out,
                ") {}",
                ColumnDefinitions {
                    columns: &self.table.column_data,
                    with_docs: self.with_docs,
                    table_full_sql_name: &full_sql_name,
                    custom_type_overrides: self.custom_type_overrides
                }
            )?;
        }
        write!(f, "}}")?;
        Ok(())
    }
}

struct ColumnDefinitions<'a> {
    columns: &'a [ColumnDefinition],
    with_docs: DocConfig,
    table_full_sql_name: &'a str,
    custom_type_overrides: Option<&'a [Option<ColumnType>]>,
}

impl<'a> Display for ColumnDefinitions<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        {
            let mut out = PadAdapter::new(f);
            writeln!(out, "{{")?;
            for (column_idx, column) in self.columns.iter().enumerate() {
                let column_type = self
                    .custom_type_overrides
                    .and_then(|ct| ct[column_idx].as_ref())
                    .unwrap_or(&column.ty);

                match self.with_docs {
                    DocConfig::NoDocComments => {}
                    DocConfig::OnlyDatabaseComments => {
                        if let Some(comment) = column.comment.as_deref() {
                            write_doc_comments(&mut out, comment)?;
                        }
                    }
                    DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment => {
                        if let Some(comment) = column.comment.as_deref() {
                            write_doc_comments(&mut out, comment)?;
                        } else {
                            write_doc_comments(
                                &mut out,
                                &format!(
                                    "The `{}` column of the `{}` table.

                                    Its SQL type is `{}`.

                                    (Automatically generated by Diesel.)",
                                    column.sql_name, self.table_full_sql_name, column_type
                                ),
                            )?;
                        }
                    }
                }

                if column.rust_name == column.sql_name {
                    writeln!(out, "{} -> {},", column.sql_name, column_type)?;
                } else {
                    writeln!(out, r#"#[sql_name = "{}"]"#, column.sql_name)?;
                    writeln!(out, "{} -> {},", column.rust_name, column_type)?;
                }
            }
        }
        writeln!(f, "}}")?;
        Ok(())
    }
}

struct Joinable<'a>(&'a ForeignKeyConstraint);

impl<'a> Display for Joinable<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let child_table_name = &self.0.child_table.rust_name;

        let parent_table_name = &self.0.parent_table.rust_name;

        write!(
            f,
            "diesel::joinable!({} -> {} ({}));",
            child_table_name, parent_table_name, self.0.foreign_key_columns_rust[0],
        )
    }
}

/// Lifted directly from libcore/fmt/builders.rs
struct PadAdapter<'a, 'b: 'a> {
    fmt: &'a mut Formatter<'b>,
    on_newline: bool,
}

impl<'a, 'b: 'a> PadAdapter<'a, 'b> {
    fn new(fmt: &'a mut Formatter<'b>) -> PadAdapter<'a, 'b> {
        PadAdapter {
            fmt,
            on_newline: false,
        }
    }
}

impl<'a, 'b: 'a> Write for PadAdapter<'a, 'b> {
    fn write_str(&mut self, mut s: &str) -> fmt::Result {
        while !s.is_empty() {
            let on_newline = self.on_newline;

            let split = match s.find('\n') {
                Some(pos) => {
                    self.on_newline = true;
                    pos + 1
                }
                None => {
                    self.on_newline = false;
                    s.len()
                }
            };

            let to_write = &s[..split];
            if on_newline && to_write != "\n" {
                self.fmt.write_str("    ")?;
            }
            self.fmt.write_str(to_write)?;

            s = &s[split..];
        }

        Ok(())
    }
}

impl<'de> Deserialize<'de> for Filtering {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FilteringVisitor;

        impl<'de> Visitor<'de> for FilteringVisitor {
            type Value = Filtering;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("either only_tables or except_tables")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut only_tables = None::<Vec<Regex>>;
                let mut except_tables = None::<Vec<Regex>>;
                while let Some(key) = map.next_key::<String>()? {
                    match &key as &str {
                        "only_tables" => {
                            if only_tables.is_some() {
                                return Err(de::Error::duplicate_field("only_tables"));
                            }
                            only_tables = Some(map.next_value()?);
                        }
                        "except_tables" => {
                            if except_tables.is_some() {
                                return Err(de::Error::duplicate_field("except_tables"));
                            }
                            except_tables = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                &key,
                                &["only_tables", "except_tables"],
                            ))
                        }
                    }
                }
                match (only_tables, except_tables) {
                    (Some(t), None) => Ok(Filtering::OnlyTables(t)),
                    (None, Some(t)) => Ok(Filtering::ExceptTables(t)),
                    (None, None) => Ok(Filtering::None),
                    _ => Err(de::Error::duplicate_field("only_tables except_tables")),
                }
            }
        }

        deserializer.deserialize_map(FilteringVisitor)
    }
}

impl DocConfig {
    pub const VARIANTS_STR: &'static [&'static str] = &[
        "database-comments-fallback-to-auto-generated-doc-comment",
        "only-database-comments",
        "no-doc-comments",
    ];
}
impl<'de> Deserialize<'de> for DocConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DocConfigVisitor;
        impl<'de> serde::de::Visitor<'de> for DocConfigVisitor {
            type Value = DocConfig;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(
                    formatter,
                    "a boolean or one of the following: {:?}",
                    DocConfig::VARIANTS_STR
                )
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(match v {
                    true => DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment,
                    false => DocConfig::NoDocComments,
                })
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(match v {
                    "database-comments-fallback-to-auto-generated-doc-comment" => {
                        DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment
                    }
                    "only-database-comments" => DocConfig::OnlyDatabaseComments,
                    "no-doc-comments" => DocConfig::NoDocComments,
                    _ => {
                        return Err(serde::de::Error::unknown_variant(
                            v,
                            DocConfig::VARIANTS_STR,
                        ))
                    }
                })
            }
        }

        deserializer.deserialize_any(DocConfigVisitor)
    }
}
impl std::str::FromStr for DocConfig {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "database-comments-fallback-to-auto-generated-doc-comment" => {
                DocConfig::DatabaseCommentsFallbackToAutoGeneratedDocComment
            }
            "only-database-comments" => DocConfig::OnlyDatabaseComments,
            "no-doc-comments" => DocConfig::NoDocComments,
            _ => {
                return Err("Unknown variant for doc config, expected one of: \
                    `database-comments-fallback-to-auto-generated-doc-comment`, \
                    `only-database-comments`, \
                    `no-doc-comments`")
            }
        })
    }
}
