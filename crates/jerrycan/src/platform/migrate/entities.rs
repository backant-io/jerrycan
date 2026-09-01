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
        let on_delete = match fk.on_delete {
            FkAction::Cascade => OnDelete::Cascade,
            FkAction::SetNull => OnDelete::SetNull,
            FkAction::Restrict => OnDelete::Restrict,
        };
        if *column == Design::fk_column(&target) {
            // The default derivation reproduces this column exactly — un-aliased.
            belongs_to.push(BelongsTo {
                entity: target,
                on_delete,
                r#as: None,
            });
            suppressed.push(column.clone());
        } else if let Some(alias) = column.strip_suffix("_id").filter(|a| {
            // Alias pattern `^[a-z][a-z0-9_]*$` — the same shape a hand-authored
            // `as` must satisfy (JC0560), so the round-trip is validation-clean.
            !a.is_empty()
                && a.starts_with(|c: char| c.is_ascii_lowercase())
                && a.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        }) {
            // Aliased fk (issue #119): a `{alias}_id` column referencing a table
            // whose default fk column would differ (`snake(target)_id != column`).
            // Round-trip it losslessly as `belongs_to { entity, as: alias }` — this
            // is exactly how two refs to one table (from_account_id/to_account_id) or
            // a self-reference (parent_id) are expressed. `belongs_to.fk_column()`
            // reproduces `column`, so an import → design → migrate loop is a fixpoint.
            belongs_to.push(BelongsTo {
                entity: target,
                on_delete,
                r#as: Some(alias.to_string()),
            });
            suppressed.push(column.clone());
        } else {
            // The column can't be expressed as an alias (no `_id` suffix, or a
            // non-snake stem) — don't guess. Keep it as a plain field and document
            // the dropped FK relation (never silently drop the reference).
            gaps.push(GapItem {
                kind: GapKind::ForeignKey,
                source: format!("{key}.{column}"),
                location: format!("schema.sql:{}", table.line),
                reason: format!(
                    "fk column `{column}` is not `{{alias}}_id`-shaped, so it can't round-trip as a belongs_to alias to `{}`",
                    Design::fk_column(&target)
                ),
                original: format!("references {}", fk.ref_table),
                suggested:
                    "rename the column to `{snake_target}_id` (or a snake `{alias}_id`) in the seed mapping, or keep it as a plain field + handler-enforced integrity"
                        .to_string(),
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
                    // Migration import derives no range/length constraints (#80);
                    // a CHECK on the source column stays a run-time concern.
                    min: None,
                    max: None,
                    min_len: None,
                    max_len: None,
                    // Migration import sets no explicit write_only (#112); a
                    // `password_hash` column is auto-hidden by the classifier.
                    write_only: false,
                    // Migration import derives no capacity reservation (#187);
                    // `reserve_against` is a design-authored wiring only.
                    reserve_against: None,
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

    // Table-level composite `UNIQUE(a, b, …)` (#115): translate each group whose
    // columns all survive as a declared field or a belongs_to fk column — the same
    // shape JC0559 accepts, so the round-trip is validation-clean. A column that
    // dropped out (e.g. an unmappable-type column) can't be indexed, so raise a
    // gap for that constraint rather than silently dropping it (never guess).
    let mut unique = Vec::new();
    for group in &table.composite_uniques {
        let missing: Vec<&str> = group
            .iter()
            .filter(|col| {
                !fields.iter().any(|f| &f.name == *col)
                    && !belongs_to.iter().any(|b| &b.fk_column() == *col)
            })
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            unique.push(group.clone());
        } else {
            gaps.push(GapItem {
                kind: GapKind::UnmappedType,
                source: format!("{key} unique({})", group.join(", ")),
                location: format!("schema.sql:{}", table.line),
                reason: format!(
                    "composite UNIQUE column(s) `{}` did not survive translation (dropped/unmapped), so the constraint can't be reproduced as a composite `unique` index",
                    missing.join(", ")
                ),
                original: format!("unique ({})", group.join(", ")),
                suggested:
                    "map the missing column(s) to a field, then add the group to the entity's `unique`, or enforce the invariant in a handler"
                        .into(),
                severity: Severity::Blocking,
            });
        }
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
        unique,
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
    fn aliased_fk_round_trips_as_belongs_to_as_and_unmappable_types_still_gap() {
        let out = build();
        let item = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "OrderItem")
            .map(|(_, e)| e)
            .unwrap();
        // author_id references workspaces but snake(Workspace)_id == workspace_id ≠
        // author_id, so it round-trips (issue #119) as `belongs_to Workspace as
        // author` — a SECOND, distinct reference to Workspace — NOT a ForeignKey gap.
        let author = item
            .belongs_to
            .iter()
            .find(|b| b.r#as.as_deref() == Some("author"))
            .expect("author_id must round-trip as `belongs_to Workspace as author`");
        assert_eq!(author.entity, "Workspace");
        assert_eq!(author.fk_column(), "author_id");
        // The aliased column is suppressed (it IS the belongs_to), not a plain field,
        // and no ForeignKey gap is raised for it (the reference is expressed, not lost).
        assert!(!item.fields.iter().any(|f| f.name == "author_id"));
        assert!(!out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::ForeignKey
            && g.source.contains("author_id")));
        // A column that cannot become a belongs_to at all (point type) still gaps as
        // an unmapped type and its field is dropped — the migrator never guesses.
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::UnmappedType
            && g.source.contains("location")));
        assert!(!item.fields.iter().any(|f| f.name == "location"));
    }

    #[test]
    fn composite_unique_survives_translation_and_an_unrepresentable_one_gaps() {
        // WHY: docs/ai/19 promises the migrator never silently drops what it can't
        // translate. A multi-column UNIQUE(a, b) IS translatable — jerrycan carries
        // it as the entity's composite `unique` (#115). It must survive; a group over
        // a dropped (unmappable-type) column must gap, not vanish.
        let schema = r#"
create table public.memberships (
    id uuid primary key,
    org_id uuid not null,
    user_id uuid not null,
    unique (org_id, user_id)
);
create table public.pins (
    id uuid primary key,
    label text not null,
    spot point,
    unique (label, spot)
);
"#;
        let db = PgDatabase::fold(&parse::split_and_parse(schema));
        let out = build_entities(&db);

        let membership = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "Membership")
            .map(|(_, e)| e)
            .unwrap();
        assert_eq!(
            membership.unique,
            vec![vec!["org_id".to_string(), "user_id".to_string()]],
            "UNIQUE(org_id, user_id) must survive as the entity's composite `unique`"
        );
        // Each column names a declared field (the shape JC0559 accepts), so the
        // group round-trips to a buildable `CREATE UNIQUE INDEX`.
        for col in &membership.unique[0] {
            assert!(membership.fields.iter().any(|f| &f.name == col));
        }

        // `spot point` is an unmappable type → dropped field + its composite unique
        // over it can't be built, so the constraint gaps rather than silently drops.
        let pin = out
            .entities
            .iter()
            .find(|(_, e)| e.name == "Pin")
            .map(|(_, e)| e)
            .unwrap();
        assert!(
            pin.unique.is_empty(),
            "a group over a dropped column must not be emitted"
        );
        assert!(
            out.gaps.iter().any(|g| g.kind
                == crate::platform::migrate::gaps::GapKind::UnmappedType
                && g.source.contains("unique(label, spot)")
                && g.reason.contains("spot")),
            "the unrepresentable composite unique must raise a gap: {:?}",
            out.gaps
        );
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
