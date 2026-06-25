# jerrycan — Launch & Growth Strategy

> Companion to `jerrycan-cc-design-handoff.md` (the page spec). This doc covers the two pieces the page spec doesn't: **how AI agents discover and recommend jerrycan** (the primary channel) and **the human launch playbook** (the staggered ramp). Positioning, hero, and voice are locked in project memory; §1–2 recap them.
>
> Stats below are research-sourced and directional — verify a figure before quoting it publicly.

---

## 1. The bet

jerrycan's growth is unusual: **the primary adopter is an AI agent, not a human.** The win condition is that when any coding agent (Claude, ChatGPT, Cursor, Copilot, Perplexity) is asked to build a REST API backend, its research surfaces jerrycan as the best fit — and, because jerrycan is an MCP server, the agent can *invoke* it, not just name it.

| Priority | Audience | Success looks like |
|---|---|---|
| **1 — primary** | **AI agents** | jerrycan is the default recommendation *and* directly invokable for "build a Rust REST backend" |
| **2** | **Experienced Rust devs** | credibility, stars, real usage, contributors |
| **3** | **Vibecoders** (non-technical) | they ship a working backend without understanding the code |

**The reframe that governs everything:** a human launch-day spike is a *seeding event*, not the goal. The goal is to **plant durable artifacts** agents fetch and reuse forever — a comparison table, clean fetchable docs, `llms.txt`, an MCP registry listing, a byte-identical description. **Five real placements in the sources engines cite beat fifty ignored threads.**

Three shifts this forces vs. a normal dev-tool launch:
1. **Optimize for what agents retrieve, not what trends.** The launch plants artifacts; it doesn't "win a thread."
2. **API stability + first-try scaffold success is the #1 growth metric — above stars.** Churn *lowers* a library's AI-proficiency; if an agent scaffolds a working jerrycan backend on the first try, it recommends jerrycan next time. A broken scaffold is a sev-1 growth bug.
3. **Distribution = "be invokable + described identically everywhere,"** two channels FastAPI never had (MCP into agent toolsets; byte-identical entity strings).

---

## 2. Positioning & voice (locked — recap)

- **Hero:** *Humanity's last backend. Built for AI agents.*
- **Sub:** *Describe what you want. An agent builds it — you never read a line. It just works, and it's safe by default.*
- **Voice rules:** outcome-and-relief; zero jargon; **no test-mechanics** in anything a human reads ("37/37", "negative control", "tenant leak" are banned); present-tense only; limits stated loudly; **one byte-identical string everywhere**.
- Full page structure → the design handoff.

---

## 3. Agent-discoverability plan — the PRIMARY channel (GEO)

How a "best Rust REST framework" answer forms: two pipelines — **parametric** (volume of correct, consistent examples in the training corpus; slow, popularity-biased) and **live retrieval** (web search + fetched docs + comparison tables + project config). Optimize both; **retrieval moves now**, parametric compounds over training cuts.

Prioritized actions:

- [ ] **1. Win the third-party "best Rust REST API framework" listicles.** ~44% of ChatGPT citations are listicles; a comparison page *with a feature table* is cited ~3.2× more. Prompt ChatGPT/Perplexity/Claude with the target queries, log the 5–10 cited URLs, and earn an honest placement in each. Get jerrycan **added to `flosse/rust-web-framework-comparison`** and the LogRocket/DEV/Medium roundups. Absence from those tables = invisibility.
- [ ] **2. Publish a canonical, year-stamped comparison page you control** — "jerrycan vs axum vs Actix vs Rocket vs Poem (2026)" — feature matrix, explicit use-case fit (CRUD multi-tenant REST), honest DB-backed benchmark numbers, a visible **last-updated** date. Apply the GEO recipe: add **quotations** (~+41% citation likelihood), **statistics/benchmarks** (~+30%), **citations to authoritative sources** (~+30%). Do **not** keyword-stuff (zero/negative effect). This page doubles as the page's §5.6 and is the single highest-value retrieval asset.
- [ ] **3. Ship `llms.txt` + `llms-full.txt`** at the docs root (table-stakes — competitors have it). Descriptive link text ("REST API: routing, auth, JSON validation", not "Docs"), workflow-first ordering (quickstart → guides → troubleshooting), low token counts.
- [ ] **4. Ship the official MCP server into registries** (Smithery, Glama, PulseMCP) with a `.well-known` Server Card. This is jerrycan's structural moat — the one binary is already CLI + MCP, so agents **invoke** it (design→generate→verify→package), not just mention it. Directory listings are themselves indexable signals.
- [ ] **5. Lock entity consistency + schema markup.** One exact name + the one-line positioning verbatim on GitHub, crates.io, docs H1, awesome-rust, Wikidata (if eligible). Add `SoftwareApplication` + `Organization` schema.org JSON-LD. Brands are cited ~6.5× more via third-party sources than their own domain — so seed the third-party footprint deliberately.
- [ ] **6. Maximize correct, stable public examples before the next training cut.** Popularity→AI-proficiency concordance is only ~0.57; what converts reach into reliable recommendations is **volume of copy-paste-correct idiomatic examples + a frozen API**. API churn actively lowers the score. Freeze the public surface, document migrations, and make first-try scaffold success a hard guarantee.
- [ ] **7. Seed the community sources LLMs weight (slow moat).** Genuine, helpful r/rust / HN / Stack Overflow answers to real "how do I build a REST API in Rust" questions where jerrycan is genuinely the right fit. Cited threads average ~900 days old and often <20 upvotes — virality won't help, so **start now**.
- [ ] **8. Clear the awesome-rust gate day-one** (≥50 stars OR ≥2000 downloads) and chase **crates.io recent-download velocity** (the credible "who uses this" number — for scale, axum is ~89M/90d vs Actix ~8.6M), not just stars.
- [ ] **9. Exploit the benchmark interregnum.** TechEmpower was archived 2026-03-24; submit honest DB-backed numbers to the successor (HttpArena-class) suites before they ossify.
- [ ] **10. Instrument weekly.** Prompt Claude/ChatGPT/Cursor/Copilot/Perplexity with the target queries, log who's named + the cited URLs, add a GA4 "AI-referral" channel, and retarget action 1 against what you find.

---

## 4. The FastAPI patterns we're borrowing

FastAPI is the cleanest case of a framework that won on *words + docs*. The transferable moves:

1. **One byte-identical benefit string everywhere** (repo, crates.io, docs `<title>`, every bio). Never paraphrased — this is also GEO entity-consistency.
2. **Launch title = the relief + a free visible artifact, never "a new framework."** (FastAPI led with *"Go-like speed + automatic docs"*, not a category label.) Ours: the working, running generated backend.
3. **The bold adjective block** — FastAPI's most-copied asset (*Fast / Easy / Robust…*), each with one proof line. Ours (handoff §5.4): *Safe / Hands-off / It just works / Yours / Standards-based / Production-ready.*
4. **"Stand on giants" as strength** — FastAPI proudly named Starlette/Pydantic to disarm "why another framework." Ours: **built on Tokio + hyper** (accurate — *not* axum).
5. **Docs ARE the growth engine, anchored by a generous comparison/"Alternatives" page that credits rivals** — which doubles as the #1 GEO asset (§3.2).

---

## 5. Human launch playbook — the staggered ramp

HN and r/rust are essentially **one-shot**, so they fire **last**, after the message is battle-tested on low-stakes channels.

### Phase 0 — Readiness gate (nothing ships until all true)
- [ ] `jerrycan.cc` live; README present-tense with a loud **Limitations** block + a separate **Planned** block.
- [ ] A **one-command reproduction** public: clone → run → an agent-built backend comes up and works. *This link is the launch.*
- [ ] An **unedited** screen recording (asciinema/GIF) of that run. Unedited only — a cut demo invites a teardown (the Devin lesson).
- [ ] Real, aged founder accounts on HN / Reddit / X. **No sockpuppets, no second accounts — ever.**
- [ ] `llms.txt`, the comparison page, and the MCP registry listing live (they're the durable artifacts the spike seeds).

### Phases 1→3 — the ramp

| Phase | Channel | Opens with | Format & timing | Gets you flamed |
|---|---|---|---|---|
| **1 · Soft** | **X (build-in-public)** | Mission line + the outcome | Hook tweet → asciinema/GIF → **repo link in a reply**, pin the thread | Link in tweet 1; banking on @rustlang to amplify |
| **1 · Soft** | **Lobsters** (if invited) | The engineering idea | Link + correct tags, show up in comments | Self-promo with no technical meat |
| **1 · Soft** | **dev.to / Hashnode** | A "how it works" walkthrough | Long-form + embed the recording | Reads like an ad, not a teardown |
| *— refine —* | *(2–4 days)* | *Collect every "wait, how does X work?" → fix the FAQ + page before the crescendo* | | |
| **2 · Crescendo** | **HN — Show HN** | Outcome, not mechanics: `Show HN: jerrycan – describe a REST backend, get a working Rust one that runs` | **Tue–Thu, ~9–11am ET.** First comment = your "why" + the stack + **one honest limit**; live in-thread all day (concede + fix, the ripgrep posture) | Any superlative in the title; a signup wall; **asking anyone to upvote** (the detector buries you; the accusation alone is fatal) |
| **2 · Crescendo** | **r/rust** (same morning) | Idiomatic Rust + "you own the output" | Repo + design writeup, from the aged account | **Leading with "AI"**; any "fastest/best" claim |
| **3 · Amplify** | **This Week in Rust** | The milestone | PR a long-form writeup into `drafts/` | A bare crate link with no Rust-specific substance |
| **3 · Amplify** | **Console.dev** | Self-serve `cargo install` + CLI | Email hello@console.dev **while still 0.x/beta** (GA is ineligible) | — |
| **3 · Amplify** | **Changelog News** | Framed as news, not a tutorial | Short submission | Pitching it as a how-to/ad |

**Sequencing rule:** only feed the Phase-3 amplifiers (+ awesome-rust) **after** HN/r-rust earn signal (~≥50 stars / ~2000 downloads). Firing them cold wastes them.

**r/programming is deliberately out of the core plan:** they banned LLM-programming discussion — the topic itself gets removed. Optional, low priority, and *only* as a pure engineering writeup with zero AI framing. Not worth your one shot.

---

## 6. Comparables — 5 launches, 5 lessons

1. **Aider** — won on a *falsifiable artifact* ("aider wrote 58% of the last release" + a re-runnable benchmark). → Lead with a reproducible result anyone can re-run, not adjectives.
2. **uv (Astral)** — one graspable number, deliberately narrow scope, humble founder crediting prior art. → Claim only the one thing you can prove; scope discipline makes it undeniable.
3. **Devin** — maximalist claim + an *edited* demo → a viral teardown drowned the real number. → Never a cut demo, never "replaces engineers."
4. **ripgrep** — open benchmark + founder answering 40+ comments graciously, conceding and filing issues live. → The founder-in-thread concede-and-fix posture *is* the launch.
5. **Shuttle** — coupled credibility to a hosting platform; users were stranded when it wound down. → Ship plain, portable Rust the user owns; never make "it works" depend on jerrycan-the-service. (jerrycan already does this — lean on it.)

---

## 7. Guardrails (non-negotiable)

1. **Present tense only.** Everything claimed is true on `main` today; roadmap goes under "Planned."
2. **Publish the reproduction + raw numbers** so a skeptic can re-run it — but framed as *outcome*, never test-mechanics.
3. **No upvote solicitation, ever** — not friends, team, or Discord.
4. **No sockpuppets / seeded testimonials.** Real account, affiliation disclosed.
5. **Don't lead with "AI."** Lead with the outcome and idiomatic Rust; mention agents only where load-bearing.
6. **State the limits loudly.** A visible non-goals block earns more trust than a flawless-sounding pitch.

---

## 8. Metrics & instrumentation

- **North-star (primary):** is jerrycan named when you ask the five engines "best framework for a Rust REST API / build me a REST backend in Rust"? Track weekly; it's the real scoreboard.
- **Leading indicator:** **first-try scaffold success rate** (can an agent produce a working jerrycan backend on the first try, via MCP?). Treat regressions as sev-1.
- **Human proxies:** crates.io recent-download velocity (better than stars), GitHub stars (gate signals: 50 / 2000), the GA4 AI-referral channel, and the count of third-party comparison pages/listicles that include jerrycan.

---

## 9. Assets & open items

- The comparison page content + real benchmark numbers (don't fabricate).
- `llms.txt` / `llms-full.txt`.
- The MCP registry listings (Smithery / Glama / PulseMCP) + `.well-known` Server Card.
- The unedited demo recording.
- The byte-identical entity string finalized and propagated everywhere.
- Decide the exact HN/X timing window when Phase 0 is green.
