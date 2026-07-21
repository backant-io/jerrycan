//! Spec §Deterministic translator (1): CREATE TABLE → Entity/Field/belongs_to.
//! Reserved-schema tables (auth/storage/cron/…) are handled by their own
//! mappers; this stage only sees `public` tables the caller passes in.

use super::gaps::{GapItem, GapKind, Severity};
use super::pgmodel::{FkAction, PgDatabase, PgTable};
use super::typemap::{MappedType, map_pg_type};
use crate::platform::design::{BelongsTo, Design, Entity, Field, FieldType, OnDelete};

pub struct BuildResult {
    /// (source "schema.table", entity) — table key kept for seeding + grouping.
    pub entities: Vec<(String, Entity)>,
    pub gaps: Vec<GapItem>,
}

const IRREGULAR: &[(&str, &str)] = &[
    ("people", "person"),
    ("children", "child"),
    ("statuses", "status"),
];

fn singularize(word: &str) -> String {
    for (plural, singular) in IRREGULAR {
        if word == *plural {
            return (*singular).to_string();
        }
    }
    if let Some(stem) = word.strip_suffix("ies") {
        return format!("{stem}y");
    }
    if let Some(stem) = word.strip_suffix("ses") {
        return format!("{stem}s");
    }
    if word.len() > 1 && word.ends_with('s') && !word.ends_with("ss") {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

/// "order_items" → "OrderItem" (last segment singularized, all PascalCased).
pub fn entity_name(table: &str) -> String {
    let segments: Vec<&str> = table.split('_').collect();
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        let seg = if i == segments.len() - 1 {
            singularize(seg)
        } else {
            (*seg).to_string()
        };
        let mut chars = seg.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

pub fn build_entities(db: &PgDatabase) -> BuildResult {
    build_entities_filtered(db, &std::collections::BTreeSet::new())
}

/// Build entities for every `public.*` table except those in `exclude` (the
/// membership table is represented by tenancy, so it's excluded by the orchestrator).
pub fn build_entities_filtered(
    db: &PgDatabase,
    exclude: &std::collections::BTreeSet<String>,
) -> BuildResult {
    let mut entities = Vec::new();
    let mut gaps = Vec::new();
    for (key, table) in &db.tables {
        if !key.starts_with("public.") || exclude.contains(key) {
            continue;
        }
        if let Some(entity) = build_one(key, table, db, &mut gaps) {
            entities.push((key.clone(), entity));
        }
    }
    BuildResult { entities, gaps }
}

fn build_one(
    key: &str,
    table: &PgTable,
    db: &PgDatabase,
    gaps: &mut Vec<GapItem>,
) -> Option<Entity> {
    if table.pk.len() > 1 {
        gaps.push(GapItem {
            kind: GapKind::UnmappedType,
            source: key.to_string(),
            location: format!("schema.sql:{}", table.line),
            reason: "composite primary key".into(),
            original: format!("primary key ({})", table.pk.join(", ")),
            suggested: "model the join table as its own entity with a surrogate id".into(),
            severity: Severity::Blocking,
        });
        return None;
    }

    let name = entity_name(&table.name);
    let mut belongs_to = Vec::new();
    let mut suppressed: Vec<String> = Vec::new();

    for fk in &table.fks {
        if fk.columns.len() != 1 {
            continue;
        }
        let column = &fk.columns[0];
        let target_table = fk.ref_table.rsplit('.').next().unwrap_or(&fk.ref_table);
        let target = entity_name(target_table);
        if *column == Design::fk_column(&target) {
            belongs_to.push(BelongsTo {
                entity: target,
                on_delete: match fk.on_delete {
                    FkAction::Cascade => OnDelete::Cascade,
                    FkAction::SetNull => OnDelete::SetNull,
                    FkAction::Restrict => OnDelete::Restrict,
                },
            });
            suppressed.push(column.clone());
        } else {
            gaps.push(GapItem {
                kind: GapKind::ForeignKey,
                source: format!("{key}.{column}"),
                location: format!("schema.sql:{}", table.line),
                reason: format!(
                    "fk column `{column}` does not match the derived belongs_to column `{}`",
                    Design::fk_column(&target)
                ),
                original: format!("references {}", fk.ref_table),
                suggested: format!(
                    "rename the column to {} in the seed mapping or keep it as a plain field + handler-enforced integrity",
                    Design::fk_column(&target)
                ),
                severity: Severity::Advisory,
            });
        }
    }

    let mut fields = Vec::new();
    for col in &table.columns {
        if suppressed.contains(&col.name) {
            continue;
        }
        match map_pg_type(&col.pg_type, &db.enums) {
            MappedType::Field { field_type, values } => {
                fields.push(Field {
                    name: col.name.clone(),
                    field_type,
                    required: col.not_null,
                    unique: col.unique,
                    index: col.indexed,
                    values: col.check_in_values.clone().or(values),
                    // Migration import derives no server-owned default from a
                    // Postgres column default (that stays a run-time concern).
                    default: None,
                });
            }
            MappedType::Unmappable { pg_type, reason } => {
                gaps.push(GapItem {
                    kind: GapKind::UnmappedType,
                    source: format!("{key}.{}", col.name),
                    location: format!("schema.sql:{}", table.line),
                    reason: reason.to_string(),
                    original: pg_type,
                    suggested: "model as string/json or a separate entity; update the seed mapping"
                        .into(),
                    severity: Severity::Blocking,
                });
            }
        }
    }

    // The `id` column becomes the primary key: questions.rs requires it map to
    // integer/string/uuid. A bad pk type is a blocking gap; the table is skipped.
    if let Some(id) = fields.iter().find(|f| f.name == "id")
        && !matches!(
            id.field_type,
            FieldType::Integer | FieldType::String | FieldType::Uuid
        )
    {
        gaps.push(GapItem {
            kind: GapKind::UnmappedType,
            source: format!("{key}.id"),
            location: format!("schema.sql:{}", table.line),
            reason: "the id column becomes the primary key and must be integer/string/uuid".into(),
            original: format!("{:?}", id.field_type),
            suggested: "change the pk type or model a surrogate id".into(),
            severity: Severity::Blocking,
        });
        return None;
    }

    // Preserve the source table name losslessly: pin `table` only when the
    // default (snake_case + pluralization) would NOT reproduce it, so a clean
    // name stays override-free while an irregular one round-trips exactly.
    let table = (Design::default_table_name(&name) != table.name).then(|| table.name.clone());

    Some(Entity {
        name,
        table,
        belongs_to,
        public_read: false,
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::{FieldType, OnDelete};
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const SCHEMA: &str = r#"
create table public.workspaces (id uuid primary key, name text not null);
create table public.order_items (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    author_id uuid references public.workspaces(id),
    label text not null check (label in ('a', 'b')),
    qty integer,
    location point
);
"#;

    fn build() -> BuildResult {
        let db = PgDatabase::fold(&parse::split_and_parse(SCHEMA));
        build_entities(&db)
    }

    #[test]
    fn a_table_becomes_a_singular_pascal_entity_with_mapped_fields() {
        let out = build();
        let item = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "OrderItem")
            .map(|(_, e)| e)
            .unwrap();
        assert_eq!(
            item.fields
                .iter()
                .find(|f| f.name == "id")
                .unwrap()
                .field_type,
            FieldType::Uuid
        );
        let label = item.fields.iter().find(|f| f.name == "label").unwrap();
        assert_eq!(
            label.values.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
        let qty = item.fields.iter().find(|f| f.name == "qty").unwrap();
        assert!(!qty.required, "nullable column → required: false");
    }

    #[test]
    fn matching_fk_becomes_belongs_to_and_the_column_is_suppressed() {
        let out = build();
        let item = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "OrderItem")
            .map(|(_, e)| e)
            .unwrap();
        let bt = item
            .belongs_to
            .iter()
            .find(|b| b.entity == "Workspace")
            .unwrap();
        assert_eq!(bt.on_delete, OnDelete::Cascade);
        // workspace_id is derived by belongs_to — an explicit field would fail questions.rs.
        assert!(!item.fields.iter().any(|f| f.name == "workspace_id"));
    }

    #[test]
    fn mismatched_fk_names_and_unmappable_types_gap_instead_of_guessing() {
        let out = build();
        // author_id references workspaces but snake(Workspace)_id == workspace_id ≠ author_id.
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::ForeignKey
            && g.source.contains("author_id")));
        // point column → unmapped_type gap, field dropped.
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::UnmappedType
            && g.source.contains("location")));
        let item = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "OrderItem")
            .map(|(_, e)| e)
            .unwrap();
        assert!(!item.fields.iter().any(|f| f.name == "location"));
    }

    #[test]
    fn naming_helpers_are_deterministic() {
        assert_eq!(entity_name("order_items"), "OrderItem");
        assert_eq!(entity_name("companies"), "Company");
        assert_eq!(entity_name("statuses"), "Status");
        assert_eq!(entity_name("people"), "Person");
        assert_eq!(entity_name("workspace"), "Workspace");
    }

    #[test]
    fn source_table_name_is_preserved_losslessly_only_when_the_default_would_drift() {
        // WHY: the importer must reproduce the SOURCE DB's exact table names.
        // A name the default table rule round-trips (workspaces → Workspace →
        // workspaces) stays override-free; an irregular plural the default would
        // NOT reproduce (people → Person → persons ≠ people) gets a pinned
        // `table` override so the generated schema matches the source exactly.
        let schema = r#"
create table public.people (id uuid primary key, name text not null);
create table public.workspaces (id uuid primary key, name text not null);
"#;
        let db = PgDatabase::fold(&parse::split_and_parse(schema));
        let out = build_entities(&db);
        let person = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "Person")
            .unwrap();
        assert_eq!(
            person.1.table.as_deref(),
            Some("people"),
            "irregular plural must be pinned so the table stays `people`, not `persons`"
        );
        let ws = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "Workspace")
            .unwrap();
        assert!(
            ws.1.table.is_none(),
            "a name the default rule reproduces needs no override"
        );
    }
}
