//! Spec §6: storage/buckets.json + storage.objects policies → the design
//! `storage` block. Recognizable owner/prefix/tenant policies configure a
//! bucket; anything else gap-reports and the bucket stays private + guarded
//! (secure default — never guessed).

use super::gaps::{GapItem, GapKind, Severity};
use super::pgmodel::PgDatabase;
use super::rls::{Recognized, Scope, find_bucket, recognize};
use crate::platform::design::{BucketDesign, StorageDesign, Visibility};
use serde::Deserialize;

/// A translated bucket with string-typed fields (decoupled from the design enum
/// so tests read naturally); converted to `BucketDesign` by `to_design`.
#[derive(Debug)]
pub struct BucketOut {
    pub name: String,
    pub visibility: String,
    pub owner: Option<String>,
    pub owner_prefix: bool,
    pub max_size: Option<String>,
    pub allowed_mime: Vec<String>,
}

#[derive(Debug)]
pub struct StorageOutput {
    pub buckets: Vec<BucketOut>,
    pub gaps: Vec<GapItem>,
}

#[derive(Debug, Deserialize)]
struct SupabaseBucket {
    #[allow(dead_code)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    public: bool,
    #[serde(default)]
    file_size_limit: Option<u64>,
    #[serde(default)]
    allowed_mime_types: Option<Vec<String>>,
}

const MB: u64 = 1024 * 1024;
const KB: u64 = 1024;

/// Human size + whether it was rounded UP to the next MB (never tighter than
/// Supabase's limit — resolved ambiguity #11).
fn human_size(bytes: u64) -> (String, bool) {
    if bytes.is_multiple_of(MB) {
        (format!("{}MB", bytes / MB), false)
    } else if bytes.is_multiple_of(KB) {
        (format!("{}KB", bytes / KB), false)
    } else {
        (format!("{}MB", bytes.div_ceil(MB)), true)
    }
}

pub fn build_storage(
    buckets_json: &str,
    db: &PgDatabase,
    user_entity: &str,
) -> Result<StorageOutput, String> {
    let raw: Vec<SupabaseBucket> = serde_json::from_str(buckets_json)
        .map_err(|e| format!("storage/buckets.json is not valid JSON: {e}"))?;
    let mut gaps = Vec::new();

    // Recognized owner/prefix scopes per bucket, folded from storage.objects policies.
    let mut owner_by_bucket: std::collections::BTreeMap<String, (Option<String>, bool)> =
        std::collections::BTreeMap::new();

    for policy in db.policies.iter().filter(|p| p.table == "storage.objects") {
        match recognize(policy) {
            Recognized::Scopes(scopes) => {
                let Some(bucket) = scopes.iter().find_map(|s| match s {
                    Scope::BucketEq { bucket } => Some(bucket.clone()),
                    _ => None,
                }) else {
                    gaps.push(policy_gap(
                        policy,
                        None,
                        "storage policy has no recognizable bucket_id filter",
                    ));
                    continue;
                };
                let entry = owner_by_bucket.entry(bucket).or_insert((None, false));
                for s in &scopes {
                    match s {
                        Scope::OwnerPrefix => {
                            entry.0 = Some(user_entity.to_string());
                            entry.1 = true;
                        }
                        Scope::Owner { .. } => entry.0 = Some(user_entity.to_string()),
                        _ => {}
                    }
                }
            }
            Recognized::Gap { reason } => {
                let bucket = find_bucket(policy);
                gaps.push(policy_gap(policy, bucket, &reason));
            }
        }
    }

    let mut buckets = Vec::new();
    for b in raw {
        let (owner, owner_prefix) = owner_by_bucket
            .get(&b.name)
            .cloned()
            .unwrap_or((None, false));
        let (max_size, rounded) = match b.file_size_limit {
            Some(bytes) => {
                let (s, r) = human_size(bytes);
                (Some(s), r)
            }
            None => (None, false),
        };
        if rounded {
            gaps.push(GapItem {
                kind: GapKind::UnmappedType,
                source: format!("storage bucket `{}` file_size_limit", b.name),
                location: "storage/buckets.json".into(),
                reason: "byte limit is not an exact MB/KB — rounded UP to the next MB (never tighter than Supabase's limit)".into(),
                original: b.file_size_limit.map(|x| x.to_string()).unwrap_or_default(),
                suggested: "set an exact max_size if the rounded cap is wrong".into(),
                severity: Severity::Advisory,
            });
        }
        buckets.push(BucketOut {
            name: b.name,
            visibility: if b.public { "public" } else { "private" }.into(),
            owner,
            owner_prefix,
            max_size,
            allowed_mime: b.allowed_mime_types.unwrap_or_default(),
        });
    }
    buckets.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(StorageOutput { buckets, gaps })
}

fn policy_gap(policy: &super::pgmodel::PgPolicy, bucket: Option<String>, reason: &str) -> GapItem {
    let target = bucket
        .map(|b| format!("{b} bucket"))
        .unwrap_or_else(|| "bucket".to_string());
    GapItem {
        kind: GapKind::RlsPolicy,
        source: format!("storage.objects policy \"{}\"", policy.name),
        location: format!("schema.sql:{}", policy.line),
        reason: reason.to_string(),
        original: policy.original.clone(),
        suggested: format!("implement as a handler guard on the {target} endpoints"),
        severity: Severity::Blocking,
    }
}

impl BucketOut {
    pub fn to_design(&self) -> BucketDesign {
        BucketDesign {
            name: self.name.clone(),
            visibility: if self.visibility == "public" {
                Visibility::Public
            } else {
                Visibility::Private
            },
            owner: self.owner.clone(),
            owner_prefix: self.owner_prefix,
            max_size: self.max_size.clone(),
            allowed_mime: self.allowed_mime.clone(),
        }
    }
}

impl StorageOutput {
    pub fn to_design(&self) -> Option<StorageDesign> {
        if self.buckets.is_empty() {
            return None;
        }
        Some(StorageDesign {
            // Default mount prefix (/storage) — no need to pin one for an import.
            base_path: None,
            buckets: self.buckets.iter().map(BucketOut::to_design).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const BUCKETS_JSON: &str = r#"[
        {"id": "avatars", "name": "avatars", "public": true,  "file_size_limit": 5242880, "allowed_mime_types": ["image/*"]},
        {"id": "invoices", "name": "invoices", "public": false, "file_size_limit": null, "allowed_mime_types": null}
    ]"#;

    const POLICIES: &str = r#"
create table public.t (id uuid primary key);
create policy avatar_owner on storage.objects for all using
    (bucket_id = 'avatars' and (storage.foldername(name))[1] = auth.uid()::text);
create policy invoice_shares on storage.objects for select using
    (bucket_id = 'invoices' and exists
        (select 1 from public.invoice_shares s where s.object_id = objects.id and s.user_id = auth.uid()));
"#;

    #[test]
    fn buckets_translate_with_visibility_size_mime_and_prefix_policy() {
        let db = PgDatabase::fold(&parse::split_and_parse(POLICIES));
        let out = build_storage(BUCKETS_JSON, &db, "User").unwrap();
        let avatars = out.buckets.iter().find(|b| b.name == "avatars").unwrap();
        assert_eq!(avatars.visibility, "public");
        assert_eq!(avatars.owner.as_deref(), Some("User"));
        assert!(
            avatars.owner_prefix,
            "foldername[1] = auth.uid() → owner_prefix"
        );
        assert_eq!(
            avatars.max_size.as_deref(),
            Some("5MB"),
            "exact byte count renders human"
        );
        assert_eq!(avatars.allowed_mime, vec!["image/*"]);
    }

    #[test]
    fn share_join_bucket_policies_gap_and_the_bucket_stays_private_guarded() {
        let db = PgDatabase::fold(&parse::split_and_parse(POLICIES));
        let out = build_storage(BUCKETS_JSON, &db, "User").unwrap();
        let invoices = out.buckets.iter().find(|b| b.name == "invoices").unwrap();
        assert_eq!(invoices.visibility, "private");
        assert!(out.gaps.iter().any(|g| g.kind
            == crate::platform::migrate::gaps::GapKind::RlsPolicy
            && g.source.contains("invoice_shares")
            && g.suggested.contains("handler guard")));
    }
}
