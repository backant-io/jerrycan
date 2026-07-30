//! pgoutput **logical** message decoding (proto_version 1): Relation / Insert /
//! Update / Delete over the raw `XLogData.data` bytes that `pgwire-replication`
//! hands us. The replication client owns the LSN, the outer XLogData/keepalive
//! frames, the standby-status feedback, and the Begin/Commit boundary events —
//! so this module is a pure, DB-free tuple decoder. Tuple columns are text
//! under proto v1.

use crate::ChangeChannelSpec;
use crate::changes::{ChangeEvent, ChangeOp};
use std::collections::HashMap;

/// Cache of Relation (`R`) messages: rel_id → (table name, column names).
#[derive(Default)]
pub(crate) struct RelationCache {
    rels: HashMap<u32, (String, Vec<String>)>,
}

/// One decoded row change, before scope keys are extracted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowChange {
    pub(crate) table: String,
    pub(crate) op: ChangeOp,
    /// Old row (update/delete under REPLICA IDENTITY FULL). None for insert.
    pub(crate) old: Option<serde_json::Value>,
    /// New row (insert/update). None for delete.
    pub(crate) new: Option<serde_json::Value>,
}

/// The outcome of decoding one logical message.
#[derive(Debug, PartialEq)]
pub(crate) enum Logical {
    /// Non-row messages (Relation, Begin, Commit, Origin, Type, Truncate,
    /// logical Message) and rows for unknown relations — nothing to deliver.
    Meta,
    Row(RowChange),
}

fn read_u16(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes(s.try_into().expect("2 bytes")))
        .ok_or_else(|| format!("pgoutput truncated (u16 at {at})"))
}

fn read_u32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes(s.try_into().expect("4 bytes")))
        .ok_or_else(|| format!("pgoutput truncated (u32 at {at})"))
}

fn read_i32(b: &[u8], at: usize) -> Result<i32, String> {
    b.get(at..at + 4)
        .map(|s| i32::from_be_bytes(s.try_into().expect("4 bytes")))
        .ok_or_else(|| format!("pgoutput truncated (i32 at {at})"))
}

/// Read a NUL-terminated C string; returns (value, offset past the NUL).
/// Bounds-checked start: a truncated frame errors (like every other reader) —
/// never an out-of-range slice panic that would kill the replication task.
fn read_cstr(b: &[u8], at: usize) -> Result<(String, usize), String> {
    let slice = b
        .get(at..)
        .ok_or_else(|| format!("pgoutput truncated (cstring start at {at})"))?;
    let end = slice
        .iter()
        .position(|&c| c == 0)
        .ok_or("pgoutput truncated (unterminated cstring)")?;
    let s = String::from_utf8_lossy(&slice[..end]).into_owned();
    Ok((s, at + end + 1))
}

/// Decode a TupleData block into a JSON object keyed by column name. `n` → null,
/// `t` → text value, `u` (unchanged TOAST) / `b` (binary) → omitted (absent).
/// Returns (row object, offset past the tuple).
fn read_tuple(
    cols: &[String],
    b: &[u8],
    at: usize,
) -> Result<(serde_json::Map<String, serde_json::Value>, usize), String> {
    let ncols = read_u16(b, at)? as usize;
    let mut pos = at + 2;
    let mut map = serde_json::Map::new();
    for i in 0..ncols {
        let kind = *b.get(pos).ok_or("pgoutput truncated (tuple kind)")?;
        pos += 1;
        let name = cols.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
        match kind {
            b'n' => {
                map.insert(name, serde_json::Value::Null);
            }
            b't' => {
                let len = read_i32(b, pos)? as usize;
                pos += 4;
                let bytes = b
                    .get(pos..pos + len)
                    .ok_or("pgoutput truncated (tuple text)")?;
                pos += len;
                map.insert(
                    name,
                    serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()),
                );
            }
            b'b' => {
                // Binary value: length-prefixed; we skip the body (proto v1 uses
                // text, so this is defensive).
                let len = read_i32(b, pos)? as usize;
                pos += 4 + len;
            }
            b'u' => {} // unchanged TOAST: value not sent — leave absent
            other => return Err(format!("pgoutput unknown tuple column kind {other:#x}")),
        }
    }
    Ok((map, pos))
}

/// Decode one logical replication message. `Relation` updates the cache; row
/// messages for an unknown relation (no cached `R` yet) decode to `Meta`.
pub(crate) fn decode_logical(b: &[u8], cache: &mut RelationCache) -> Result<Logical, String> {
    match b.first() {
        None => Err("empty logical message".into()),
        Some(b'R') => {
            let rel_id = read_u32(b, 1)?;
            let (_ns, pos) = read_cstr(b, 5)?;
            let (name, pos) = read_cstr(b, pos)?;
            // replica identity setting (1 byte), then column count.
            let ncols = read_u16(b, pos + 1)? as usize;
            let mut pos = pos + 3;
            let mut columns = Vec::with_capacity(ncols);
            for _ in 0..ncols {
                // flags (1) + name (cstr) + type oid (4) + typmod (4)
                let (col, next) = read_cstr(b, pos + 1)?;
                columns.push(col);
                pos = next + 8;
            }
            cache.rels.insert(rel_id, (name, columns));
            Ok(Logical::Meta)
        }
        Some(b'I') => {
            let rel_id = read_u32(b, 1)?;
            let Some((table, cols)) = cache.rels.get(&rel_id) else {
                return Ok(Logical::Meta);
            };
            // 'N' tuple.
            let tag = *b.get(5).ok_or("insert missing new-tuple tag")?;
            if tag != b'N' {
                return Err(format!("insert expected 'N', got {tag:#x}"));
            }
            let (new, _) = read_tuple(cols, b, 6)?;
            Ok(Logical::Row(RowChange {
                table: table.clone(),
                op: ChangeOp::Insert,
                old: None,
                new: Some(serde_json::Value::Object(new)),
            }))
        }
        Some(b'U') => {
            let rel_id = read_u32(b, 1)?;
            let Some((table, cols)) = cache.rels.get(&rel_id) else {
                return Ok(Logical::Meta);
            };
            let mut pos = 5;
            let mut old = None;
            let tag = *b.get(pos).ok_or("update missing tuple tag")?;
            if tag == b'K' || tag == b'O' {
                let (o, next) = read_tuple(cols, b, pos + 1)?;
                old = Some(serde_json::Value::Object(o));
                pos = next;
                let ntag = *b.get(pos).ok_or("update missing new tuple after old")?;
                if ntag != b'N' {
                    return Err(format!("update expected 'N' after old, got {ntag:#x}"));
                }
                pos += 1;
            } else if tag == b'N' {
                pos += 1;
            } else {
                return Err(format!("update unexpected tuple tag {tag:#x}"));
            }
            let (new, _) = read_tuple(cols, b, pos)?;
            Ok(Logical::Row(RowChange {
                table: table.clone(),
                op: ChangeOp::Update,
                old,
                new: Some(serde_json::Value::Object(new)),
            }))
        }
        Some(b'D') => {
            let rel_id = read_u32(b, 1)?;
            let Some((table, cols)) = cache.rels.get(&rel_id) else {
                return Ok(Logical::Meta);
            };
            let tag = *b.get(5).ok_or("delete missing tuple tag")?;
            if tag != b'K' && tag != b'O' {
                return Err(format!("delete expected 'K'/'O', got {tag:#x}"));
            }
            let (old, _) = read_tuple(cols, b, 6)?;
            Ok(Logical::Row(RowChange {
                table: table.clone(),
                op: ChangeOp::Delete,
                old: Some(serde_json::Value::Object(old)),
                new: None,
            }))
        }
        // Begin/Commit arrive as separate pgwire events; Origin/Type/Truncate/
        // logical-Message carry nothing we deliver.
        Some(_) => Ok(Logical::Meta),
    }
}

impl RowChange {
    /// Extract the scope keys for this change against the channel spec. Returns
    /// None only if neither tuple carries the primary key (a decode we skip).
    pub(crate) fn into_event(self, spec: &ChangeChannelSpec) -> Option<ChangeEvent> {
        let get = |row: &Option<serde_json::Value>, col: &str| -> Option<String> {
            row.as_ref().and_then(|v| v.get(col)).and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            })
        };
        let pk = get(&self.new, &spec.pk_column).or_else(|| get(&self.old, &spec.pk_column))?;
        let (tenant_id, old_tenant_id) = match &spec.tenant_column {
            Some(col) => (get(&self.new, col), get(&self.old, col)),
            None => (None, None),
        };
        Some(ChangeEvent {
            entity: spec.entity.clone(),
            op: self.op,
            pk,
            row: self.new,
            tenant_id,
            old_tenant_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Relation ('R') message for a table.
    fn relation_msg(rel_id: u32, name: &str, cols: &[&str]) -> Vec<u8> {
        let mut b = vec![b'R'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.extend_from_slice(b"public\0"); // namespace
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        b.push(b'f'); // replica identity: full
        b.extend_from_slice(&(cols.len() as u16).to_be_bytes());
        for c in cols {
            b.push(0); // flags
            b.extend_from_slice(c.as_bytes());
            b.push(0);
            b.extend_from_slice(&25u32.to_be_bytes()); // type oid (text)
            b.extend_from_slice(&(-1i32).to_be_bytes()); // typmod
        }
        b
    }

    /// TupleData: n columns of ('t', len, text) / 'n' null.
    fn tuple(vals: &[Option<&str>]) -> Vec<u8> {
        let mut b = (vals.len() as u16).to_be_bytes().to_vec();
        for v in vals {
            match v {
                Some(s) => {
                    b.push(b't');
                    b.extend_from_slice(&(s.len() as i32).to_be_bytes());
                    b.extend_from_slice(s.as_bytes());
                }
                None => b.push(b'n'),
            }
        }
        b
    }

    fn insert_msg(rel_id: u32, vals: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'I'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'N');
        b.extend_from_slice(&tuple(vals));
        b
    }

    fn update_msg_with_old(rel_id: u32, old: &[Option<&str>], new: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'U'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'O'); // OLD tuple follows (REPLICA IDENTITY FULL)
        b.extend_from_slice(&tuple(old));
        b.push(b'N');
        b.extend_from_slice(&tuple(new));
        b
    }

    fn delete_msg(rel_id: u32, old: &[Option<&str>]) -> Vec<u8> {
        let mut b = vec![b'D'];
        b.extend_from_slice(&rel_id.to_be_bytes());
        b.push(b'O');
        b.extend_from_slice(&tuple(old));
        b
    }

    #[test]
    fn relation_then_insert_yields_named_row() {
        let mut cache = RelationCache::default();
        assert!(matches!(
            decode_logical(
                &relation_msg(1, "lead", &["id", "workspace_id"]),
                &mut cache
            )
            .unwrap(),
            Logical::Meta
        ));
        let Logical::Row(row) =
            decode_logical(&insert_msg(1, &[Some("42"), Some("7")]), &mut cache).unwrap()
        else {
            panic!("expected a row")
        };
        assert_eq!(row.table, "lead");
        assert_eq!(row.op, ChangeOp::Insert);
        assert_eq!(row.new.as_ref().unwrap()["id"], "42");
        assert_eq!(row.new.as_ref().unwrap()["workspace_id"], "7");
        assert!(row.old.is_none());
    }

    #[test]
    fn update_carries_old_and_new_tuples() {
        let mut cache = RelationCache::default();
        decode_logical(
            &relation_msg(1, "lead", &["id", "workspace_id"]),
            &mut cache,
        )
        .unwrap();
        let Logical::Row(row) = decode_logical(
            &update_msg_with_old(1, &[Some("42"), Some("3")], &[Some("42"), Some("7")]),
            &mut cache,
        )
        .unwrap() else {
            panic!("expected a row")
        };
        assert_eq!(row.old.as_ref().unwrap()["workspace_id"], "3");
        assert_eq!(row.new.as_ref().unwrap()["workspace_id"], "7");
    }

    #[test]
    fn delete_carries_only_old() {
        let mut cache = RelationCache::default();
        decode_logical(
            &relation_msg(1, "lead", &["id", "workspace_id"]),
            &mut cache,
        )
        .unwrap();
        let Logical::Row(row) =
            decode_logical(&delete_msg(1, &[Some("42"), Some("7")]), &mut cache).unwrap()
        else {
            panic!("expected a row")
        };
        assert_eq!(row.op, ChangeOp::Delete);
        assert!(row.new.is_none());
        assert_eq!(row.old.as_ref().unwrap()["id"], "42");
    }

    #[test]
    fn truncated_relation_message_errors_instead_of_panicking() {
        // A Relation header cut off before its namespace cstring used to index
        // `b[at..]` out of range and panic — killing the whole replication
        // task. It must be a clean decode error instead.
        let mut cache = RelationCache::default();
        assert!(decode_logical(&[b'R', 0, 0, 0, 1], &mut cache).is_err());
        // Cut off mid column list (claims 2 columns, supplies none).
        let mut b = vec![b'R'];
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"public\0lead\0");
        b.push(b'f');
        b.extend_from_slice(&2u16.to_be_bytes());
        assert!(decode_logical(&b, &mut cache).is_err());
    }

    #[test]
    fn begin_commit_and_unknown_tables_are_meta_or_skipped() {
        let mut cache = RelationCache::default();
        // Begin ('B') and Commit ('C') are delivered as separate pgwire events,
        // so within XLogData they never appear — but if they did, they're Meta.
        let mut begin = vec![b'B'];
        begin.extend_from_slice(&[0u8; 20]);
        assert_eq!(decode_logical(&begin, &mut cache).unwrap(), Logical::Meta);
        // An insert for an unseen relation id is skipped, not a crash.
        assert!(matches!(
            decode_logical(&insert_msg(99, &[Some("1")]), &mut cache).unwrap(),
            Logical::Meta
        ));
    }

    #[test]
    fn row_change_becomes_change_event_with_scope_keys() {
        let spec = ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
            hidden_columns: Vec::new(),
        };
        let row = RowChange {
            table: "lead".into(),
            op: ChangeOp::Update,
            old: Some(serde_json::json!({"id": "42", "workspace_id": "3"})),
            new: Some(serde_json::json!({"id": "42", "workspace_id": "7"})),
        };
        let ev = row.into_event(&spec).unwrap();
        assert_eq!(ev.pk, "42");
        assert_eq!(ev.tenant_id.as_deref(), Some("7"));
        assert_eq!(ev.old_tenant_id.as_deref(), Some("3"));
        assert_eq!(ev.row.as_ref().unwrap()["workspace_id"], "7");
    }
}
