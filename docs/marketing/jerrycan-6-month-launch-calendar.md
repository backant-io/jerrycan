# jerrycan — 6-Month Launch Calendar (operating rhythm)

> Turns `jerrycan-launch-strategy.md` into a daily/weekly/monthly cadence. Window: **2026-06-25 → 2026-12.** Built for a **solo founder who is also building the framework** — so it separates a heavier **build phase** (product + artifacts) from a sustainable **steady-state cadence** (~45–60 min/day) once live.

## The four rules that override the calendar

1. **Everything is gated on an API-stability milestone — this is the real precondition.** jerrycan is mid-0.2.x. Every durable artifact (examples, comparison page, scaffold guarantee, the "clone-and-run *is* the launch") assumes a *frozen, reliable* public surface. **If the API is still moving, seeding examples actively damages the primary channel** — churn lowers a library's AI-proficiency, so a moving target means your marketing decays faster than you can ship it. **Do not start the human ramp until you can commit to a frozen public API for the launch window.** Targets below are aim points *behind this gate*, not deadlines.
2. **The launch is gate-driven, not date-driven.** Don't fire the one-shot channels (HN, r/rust) until the readiness gate (product + artifacts) is green — even if it slips weeks.
3. **Agents are the primary audience.** When time is tight, durable artifacts + the scaffold-success harness beat chasing a human spike. **Vibecoders (audience #3) have no direct channel here — they arrive when their agent recommends jerrycan inside their tool. That *is* the agent-discoverability play; the only vibecoder surface we own is the zero-jargon page hero.**
4. **Guardrails are operating rules, not suggestions** (bottom of doc).

---

## Part A — The recurring engine (steady-state, runs every week once live)

This is the part that compounds. Monthly themes (Part B) layer on top. ~45–60 min/day + one half-day/week.

### Daily (~30–45 min — never manufacture activity)
- [ ] **Run the scaffold-success harness** (the one metric you can move before anyone shows up). An automated job that feeds N representative prompts through agent→`jerrycan` scaffold→build and records a **pass rate**. A drop is a **sev-1 product bug**, not a marketing problem. *Instrument this actively — early on you have no users to report failures.*
- [ ] **Inbox sweep** — GitHub issues/discussions, X mentions, new stars: respond, thank, file.
- [ ] **Seed an answer ONLY when a genuinely good-fit question appears** (expect **1–3/week, not daily** — a daily quota forces square-peg posts, which is the exact astroturf the guardrails forbid). One genuinely helpful answer beats ten thin ones.

### Weekly (one half-day block)
- [ ] **GEO scoreboard — done rigorously.** For each of the 5 engines (Claude, ChatGPT, Cursor/Copilot, Perplexity) run each target query **5× in a clean/incognito session** — *"best framework for a REST API in Rust"*, *"build me a multi-tenant REST backend in Rust"*. Record two numbers: **named-rate** (fraction of runs naming jerrycan — noisy, parametric) and **cited-URL presence** (does jerrycan's page/comparison appear in cited sources — *deterministic, retrieval-based, this is the trustworthy leading metric*). Don't trust a 1-of-10 binary; trust the cited-URL trend.
- [ ] **Ship one durable artifact** — a docs page, a comparison-table row, an `llms.txt` tweak, one copy-paste-correct example.
- [ ] **Write one piece of content** — a "how to build X with jerrycan", a "vs Y" section, a teardown. Feeds humans *and* retrieval.
- [ ] **Metrics snapshot** (the one sheet below).

### Monthly (half-day)
- [ ] **Retro against the cited-URL trend.** Where is jerrycan still invisible in retrieval? That gap sets next month's theme.
- [ ] **One flagship artifact** (comparison page upgrade, benchmark submission, a substantial example app).
- [ ] **One outreach batch** for the stage (TWiR, a newsletter, a podcast, an awesome-list PR).
- [ ] **Refresh the comparison page** — bump the visible "last updated" date + numbers.

---

## Part B — The 6-month arc

| Month | Theme | Definition of done |
|---|---|---|
| **1 · Jul** | **API-stability + the two core artifacts** | API freeze committed; comparison page v1 + `llms.txt` live; scaffold harness running; baseline scoreboard |
| **2 · Aug** | **Finish readiness → soft-launch** | MCP listed, demo, page live; soft channels seeded + message refined; owned channel exists |
| **3 · Sep** | **The crescendo (gate-driven)** | r/rust then HN fired; flop-or-not decision made honestly |
| **4 · Oct** | **Compound + widen retrieval** | evergreen examples; benchmark live; in 3rd-party comparisons |
| **5 · Nov** | **Authority + contributors** | a flagship piece; contributor flywheel turning |
| **6 · Dec** | **Measure & plan** | 6-month retro; next-half plan from the data |

> **Readiness was split across two months on purpose.** Doing "page + llms.txt + comparison page + MCP-in-3-registries + API freeze + scaffold guarantee + demo" in 4 weeks *while building the framework* is not a 1-hr/day job — it's the build phase. The product-dependent items (API freeze, scaffold guarantee, MCP listing) are gated on the **product**, not the calendar.

### Month 1 (July) — API-stability + the two artifacts that don't need the product frozen.
- [ ] **Decide and commit the API-freeze plan** (rule #1). If it can't freeze yet, the launch slips — that's the correct call, not a failure.
- [ ] **Comparison page v1** (`jerrycan vs axum/Actix/Rocket/Poem`) — ship it **without a benchmark row, explicitly labeled "benchmarks coming"** (don't block the highest-value asset on numbers you don't have yet). Build it to the GEO recipe **and** for human Google SEO ("rust rest framework") — same page, free durable traffic.
- [ ] **`llms.txt` + `llms-full.txt`**.
- [ ] **Stand up the scaffold-success harness** and start recording a daily pass rate.
- [ ] **Entity string** propagated everywhere (GitHub, crates.io, docs H1, bios).

### Month 2 (August) — Finish readiness, then soft-launch.
- [ ] **Product-gated artifacts** (as stability allows): `jerrycan.cc` live, **MCP server published + listed** (Smithery/Glama/PulseMCP + `.well-known` card), the **unedited demo recording**, first-try scaffold success rock-solid.
- [ ] **Stand up an owned channel** — a changelog email list or RSS on `jerrycan.cc`. Six months of rented attention (HN/Reddit/engines) needs *one* thing you control to re-reach interest.
- [ ] **Soft-launch (weeks 3–4):** X build-in-public (≤1–2 posts/week, each useful even if nobody clicks the reply link), Lobsters if invited, a dev.to walkthrough. **Collect every "wait, how does X work?" and fix the FAQ + page** before the crescendo.

### Month 3 (September) — The one-shot crescendo (only when the gate is green).
- [ ] **Stagger the one-shot channels — do not fire both the same morning** (one bad first-comment shouldn't burn both irreplaceable channels, and you can't be in two threads at once solo). **r/rust first** (lower-stakes dress rehearsal), **HN the next day** (Tue–Thu ~9–11am ET) with framing refined from the r/rust reception. Outcome-framed title; first comment = your why + stack + one honest limit; in-thread all day, concede-and-fix.
- [ ] **Flop contingency (decide honestly ~48h after):** if the gate signal misses (~<50 stars / <2000 downloads), **do NOT fire the Phase-3 amplifiers.** Diagnose (title? timing? artifact? wrong room?), and schedule **one** re-angle in 4–6 weeks (a different hook on dev.to/Lobsters). A flat launch quietly poisoned by amplifying it is worse than a flat launch you learned from.
- [ ] If the signal **hits:** awesome-rust PR, TWiR draft, Console.dev (while 0.x/beta), Changelog News.

### Month 4 (October) — Compound + widen retrieval.
- [ ] **3–4 evergreen "build X with jerrycan" examples** — the copy-paste-correct, *API-stable* examples that feed parametric recommendation (only valuable if rule #1 held).
- [ ] **First benchmark submission** (HttpArena-class) → upgrade the comparison page's benchmark row.
- [ ] Outreach to get **added to existing third-party "best Rust framework" listicles**.
- [ ] `axum → jerrycan` and `FastAPI → jerrycan` migration/"vs" guides.

### Month 5 (November) — Authority + contributors.
- [ ] A **flagship piece**: a deep technical writeup or a real case study (an agent-built backend shipped on jerrycan).
- [ ] **Open + turn the contributor flywheel:** `CONTRIBUTING.md`, good-first-issues, triage/merge/credit publicly.
- [ ] A podcast / guest post / meetup talk.
- [ ] Re-run launch-intel: double down on what moved the cited-URL trend, prune what didn't.

### Month 6 (December) — Measure, consolidate, plan.
- [ ] **6-month retro** vs the cited-URL trend + the leading indicators. Be honest about what worked.
- [ ] Consolidate best content into a canonical hub; refresh all dates/numbers.
- [ ] A "state of jerrycan" post (tie to a release if one lands).
- [ ] **Plan the next 6 months from the data.**

---

## Metrics — the one sheet

| Metric | What it really tells you | Trust level |
|---|---|---|
| **Scaffold-success pass rate** (harness) | The leading indicator you can move with zero users — converts reach into recommendations | **highest — actively instrumented** |
| **Cited-URL presence** (jerrycan in engines' cited sources) | Deterministic, retrieval-based — the real GEO scoreboard | **high** |
| **Named-rate** (fraction of 5×/engine runs naming jerrycan) | Parametric mindshare — noisy, don't over-trust a small n | medium |
| **crates.io 90-day download velocity** | Credible "who uses this" (better than stars) | medium |
| **# third-party pages including jerrycan** | Retrieval footprint | medium |
| **GitHub stars** | Gate signal for amplifiers only — vanity otherwise | low |

---

## Guardrails — operating rules (never break)

1. **Present tense only** — claim only what's true on `main`; roadmap under "Planned."
2. **Reproducible, framed as outcome** — let skeptics re-run it; never test-mechanics in human copy.
3. **No upvote solicitation, ever.** No sockpuppets, no seeded testimonials — real account, affiliation disclosed.
4. **Don't lead with "AI"** in engineer rooms — lead with the outcome and idiomatic Rust.
5. **State the limits loudly.**
6. **Seeding is opportunistic, never a quota** — if there's no genuinely good-fit question this week, post nothing.

> When a week is overwhelming, drop *content* and *outreach* first. **Never** drop the daily scaffold harness or the weekly cited-URL probe — those two are the whole game. And **never** let the calendar push you to launch before the API can freeze (rule #1): a launch onto a moving API damages the primary channel faster than any spike helps it.
