//! `jerrycan migrate --from supabase`: the deterministic translator (spec
//! 2026-07-10). Two front-ends (offline export dir, live catalogs) fold into
//! one PgDatabase IR; pure stages translate what is safe and gap-report the rest.

pub mod export;
pub mod gaps;
pub mod parse;
pub mod pgmodel;
pub mod authmap;
pub mod crud;
pub mod entities;
pub mod grouping;
pub mod cronmap;
pub mod realtimemap;
pub mod redact;
pub mod rls;
pub mod seed;
pub mod storagemap;
pub mod tenancy;
pub mod typemap;
