//! Spec §Deterministic translator (1): the Postgres → design type map.
//! Anything not in the table is Unmappable — the caller emits an
//! `unmapped_type` gap item; the type is never guessed.

use crate::platform::design::FieldType;
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum MappedType {
    Field {
        field_type: FieldType,
        values: Option<Vec<String>>,
    },
    Unmappable {
        pg_type: String,
        reason: &'static str,
    },
}

pub fn map_pg_type(pg: &str, enums: &BTreeMap<String, Vec<String>>) -> MappedType {
    // Native `CREATE TYPE … AS ENUM` columns take the same design shape an inline
    // `CHECK (col IN (…))` produces — a `String` field constrained to `values`.
    // Enum keys are always schema-qualified (CREATE TYPE defaults to `public.`),
    // but a column may reference the type unqualified (`col status`), so a bare
    // name also matches the `public.`-defaulted key.
    let labels = enums.get(pg).or_else(|| {
        enums
            .get(&format!("public.{pg}"))
            .filter(|_| !pg.contains('.'))
    });
    if let Some(labels) = labels {
        return MappedType::Field {
            field_type: FieldType::String,
            values: Some(labels.clone()),
        };
    }
    if pg.ends_with("[]") {
        return MappedType::Unmappable {
            pg_type: pg.into(),
            reason: "array types have no design representation — model as a child entity or json",
        };
    }
    let ft = match pg {
        "text" | "varchar" | "character varying" | "char" | "character" | "citext" => {
            FieldType::String
        }
        "smallint" | "int2" | "integer" | "int" | "int4" | "bigint" | "int8" | "serial"
        | "bigserial" => FieldType::Integer,
        "numeric" | "decimal" | "real" | "float4" | "double precision" | "float8" => {
            FieldType::Float
        }
        "boolean" | "bool" => FieldType::Boolean,
        "timestamp"
        | "timestamptz"
        | "timestamp with time zone"
        | "timestamp without time zone"
        | "date" => FieldType::Datetime,
        "uuid" => FieldType::Uuid,
        "json" | "jsonb" => FieldType::Json,
        _ => {
            return MappedType::Unmappable {
                pg_type: pg.into(),
                reason: "no deterministic design type for this Postgres type (composite/domain/extension type)",
            };
        }
    };
    MappedType::Field {
        field_type: ft,
        values: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::FieldType;
    use std::collections::BTreeMap;

    #[test]
    fn the_spec_type_map_holds_exactly() {
        let enums = BTreeMap::new();
        let cases = [
            ("text", FieldType::String),
            ("varchar", FieldType::String),
            ("citext", FieldType::String),
            ("int4", FieldType::Integer),
            ("integer", FieldType::Integer),
            ("bigint", FieldType::Integer),
            ("int8", FieldType::Integer),
            ("smallint", FieldType::Integer),
            ("numeric", FieldType::Float),
            ("real", FieldType::Float),
            ("double precision", FieldType::Float),
            ("boolean", FieldType::Boolean),
            ("bool", FieldType::Boolean),
            ("timestamp", FieldType::Datetime),
            ("timestamptz", FieldType::Datetime),
            ("date", FieldType::Datetime),
            ("uuid", FieldType::Uuid),
            ("json", FieldType::Json),
            ("jsonb", FieldType::Json),
        ];
        for (pg, want) in cases {
            match map_pg_type(pg, &enums) {
                MappedType::Field {
                    field_type,
                    values: None,
                } => assert_eq!(field_type, want, "{pg}"),
                other => panic!("{pg}: {other:?}"),
            }
        }
    }

    #[test]
    fn enum_types_map_to_string_with_values() {
        let mut enums = BTreeMap::new();
        enums.insert(
            "public.customer_status".to_string(),
            vec!["lead".into(), "active".into()],
        );
        match map_pg_type("public.customer_status", &enums) {
            MappedType::Field {
                field_type: FieldType::String,
                values: Some(v),
            } => {
                assert_eq!(v, vec!["lead", "active"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unqualified_column_matches_a_public_defaulted_enum_key() {
        // `CREATE TYPE status …` keys as `public.status`, but a column `col status`
        // resolves to the bare `status` — the bare name must still find the enum
        // (else the column drops as "no deterministic design type"). Issue #303.
        let mut enums = BTreeMap::new();
        enums.insert(
            "public.status".to_string(),
            vec!["active".into(), "inactive".into()],
        );
        match map_pg_type("status", &enums) {
            MappedType::Field {
                field_type: FieldType::String,
                values: Some(v),
            } => assert_eq!(v, vec!["active", "inactive"]),
            other => panic!("bare enum ref must not be unmappable: {other:?}"),
        }
    }

    #[test]
    fn arrays_composites_domains_geometry_are_unmappable_never_guessed() {
        let enums = BTreeMap::new();
        for pg in [
            "text[]",
            "public.address",
            "geometry",
            "tsvector",
            "bytea",
            "inet",
        ] {
            assert!(
                matches!(map_pg_type(pg, &enums), MappedType::Unmappable { .. }),
                "{pg} must gap"
            );
        }
    }
}
