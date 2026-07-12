//! `jerrycan migrate --from supabase`: the deterministic translator (spec
//! 2026-07-10). Two front-ends (offline export dir, live catalogs) fold into
//! one PgDatabase IR; pure stages translate what is safe and gap-report the rest.

pub mod export;
pub mod gaps;
pub mod parse;
