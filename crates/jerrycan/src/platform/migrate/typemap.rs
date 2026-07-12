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
    if let Some(labels) = enums.get(pg) {
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
        "timestamp" | "timestamptz" | "timestamp with time zone"
        | "timestamp without time zone" | "date" => FieldType::Datetime,
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
    fn arrays_composites_domains_geometry_are_unmappable_never_guessed() {
        let enums = BTreeMap::new();
        for pg in ["text[]", "public.address", "geometry", "tsvector", "bytea", "inet"] {
            assert!(
                matches!(map_pg_type(pg, &enums), MappedType::Unmappable { .. }),
                "{pg} must gap"
            );
        }
    }
}
