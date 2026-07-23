# jerrycan.cc — Promo Page Design Handoff

> **For the designer/agent:** This is a self-contained brief to design and build the jerrycan.cc landing page. The **voice, structure, and copy are locked** — treat them as source of truth. The **visual design is open** within the art direction in §6. Do not invent new claims; do not add anything in the "Do NOT" list (§9).

---

## 1. Product in one paragraph (context)

jerrycan is an AI-native Rust backend framework **and** a generation platform: one binary that an AI agent drives to **design → build → verify → package** a complete, deployable backend. A human describes what they want in plain language; an agent builds it on jerrycan; the result is real, production Rust the human never has to read. It targets CRUD-style, multi-tenant REST APIs — the backbone of most SaaS.

---

## 2. Mission & positioning (the soul of the page)

**Mission:** the last backend framework — the one built for the era where agents write the software and humans just say what they want. It just works, and it's safe by default.

**Four pillars:** (1) the *last* backend; (2) built for AI agents; (3) humans never need to understand the code; (4) it just works, *safely*.

**Locked hero headline:** **"Humanity's last backend. Built for AI agents."**
*(Punctuation is the only adjustable part — e.g. an em-dash variant. The words are fixed.)*

---

## 3. Audiences (who the page is for)

| Audience | Role | How the page serves them |
|---|---|---|
| **AI agents** (PRIMARY) | The main adoption channel — agents recommend & invoke jerrycan | Mostly a *build* concern, not visual (see §8 GEO): clean fetchable markup, a comparison page, `llms.txt`, schema.org. The page must be fast, static, semantic. |
| **Vibecoders** (human A) | Build with AI, little/no coding knowledge | Converted **above the fold** — pure outcome and relief, zero jargon. |
| **Experienced Rust devs** (human B) | Skeptical, want substance | Reassured **below the fold** — lineage, standards, the "it's real Rust you own" proof. |

**The governing principle — a two-layer page:** an emotional, zero-jargon hero that lands for the vibecoder *and* the tired senior who wants to stop writing boilerplate; then a substantive body that satisfies the skeptic and filters out no one. (This is exactly what Lovable/Supabase do: emotional hero, technical body.)

---

## 4. Voice & tone rules (non-negotiable)

- **Outcome and relief, never mechanics.** Sell what the reader *gets* and what they no longer have to *carry* — not how it works internally.
- **Zero jargon.** It must land for someone who doesn't know what "REST", "auth", or "multi-tenant" mean. Translate every term into a felt outcome.
- **BANNED words/claims on every human-facing surface:** "acceptance tests", "37/37", "negative control", "tenant data-leak", "the gate", "over HTTP", "crate-per-module". These mean nothing to a human and read as noise.
- **Present tense only.** Everything stated as fact must be true on `main` today. Anything aspirational goes under a clearly-labeled "Planned".
- **State the limits loudly** (§5.7). Naming the edges earns more trust than a flawless-sounding pitch.
- **One byte-identical string everywhere.** The headline + tagline are repeated verbatim on the page, GitHub, crates.io, and every bio — never paraphrased.

---

## 5. Page structure — section by section (copy is locked)

### 5.1 Hero
> # Humanity's last backend. Built for AI agents.
> Describe what you want. An agent builds it on jerrycan — you never read a line. It just works, and it's safe by default.
>
> <sub>Built on Tokio and hyper · real Rust you own and can read any time — you just won't need to.</sub>

Primary CTA: **Read the docs** · Secondary: **Star on GitHub**. A hero demo (see §6) sits beside or below the headline.

### 5.2 The relief (proof-as-feeling)
> The auth. The data isolation. The security holes you'd lie awake over.
> All handled — **not because you got it right. Because you never had to.**

### 5.3 How it works — three steps
1. **Describe it.** Tell jerrycan what your product does, in plain language. No schemas, no boilerplate.
2. **It builds itself.** An agent designs the backend, writes it, and proves it runs — production Rust, generated in front of you.
3. **Ship it.** A real, deployable backend you own — container, binary, or cloud — with everything a senior engineer would set up, already set up.

### 5.4 The adjective block (FastAPI's most-copied pattern — bold word + one relief line)
- **Safe** — Secure by default. The mistakes you'd make aren't yours to make.
- **Hands-off** — You describe it; an agent builds it. You never touch the code.
- **It just works** — It ships only when it actually runs. No half-built backends.
- **Yours** — Plain Rust you own. No lock-in, no platform to be trapped in.
- **Standards-based** — REST, JSON, JWT, OAuth. Nothing exotic to learn or explain.
- **Production-ready** — Auth, multi-tenancy, and deploy artifacts in the box.

### 5.5 Stand on giants
> Built on **Tokio** and **hyper** — the same foundations the Rust world already runs on. jerrycan isn't a walled garden; it's real Rust you can read any time. You just won't need to.

### 5.6 Comparison table (also the #1 agent-discoverability asset — see §8)
A feature matrix: **jerrycan vs axum vs Actix vs Rocket vs Poem.** Suggested rows: *You write the code? · Generates the backend? · Auth + multi-tenancy built in? · AI-agent native (MCP)? · You own the output? · Best for.* Must have a visible **"last updated"** date. **Do not fabricate benchmark numbers** — leave a placeholder until real DB-backed figures exist.

### 5.7 What it's for / what it's not (credibility)
> **For:** CRUD-style, multi-tenant REST APIs — the backbone of most SaaS.
> **Not (yet):** realtime/websockets, GraphQL, file storage, edge/serverless. It runs as a normal always-on service.
> We'd rather show you the edges than oversell the middle.

### 5.8 "For the skeptics" strip (engineer-facing — outcome-framed, no mechanics)
> Skeptical? Good. An AI agent built a **real multi-tenant SaaS backend** on jerrycan — clone it and run it yourself. It's idiomatic Rust; read every line if you want to.

### 5.9 Get started
```bash
# Build a backend with an agent (the CLI + MCP server)
cargo install jerrycan
# Or add the framework to a Rust app
cargo add jerrycan --features db,auth,validate,observe
```
CTAs: **Docs** · **GitHub** · **crates.io**. No signup, no email wall.

### 5.10 Footer
Repo · crates.io · docs · license (MIT) · a one-line repeat of the headline.

---

## 6. Visual & art direction (open, within this mood)

- **Mood:** confident, calm, a little *mythic*. The "Humanity's last backend" line is a manifesto — give it room to breathe: big type, generous negative space. Not loud-gradient SaaS-template soup.
- **Hero demo (important):** an **unedited** terminal recording (asciinema/GIF) of *describe → generate → running backend*. Click-to-play or a quiet muted loop. Unedited only — a cut demo reads as fake.
- **Theme:** dark-first (developer audience), high legibility. Monospace accents for code/commands; a clean humanist sans for prose.
- **Reference vibes:** FastAPI docs (clarity), Linear / Vercel (restraint), with one mythic hero moment.
- **Density:** sparse and emotional above the fold; information-dense and technical below it. The fold is the line between the vibecoder and the senior dev.
- **Color/type:** open to the designer. One restrained accent color; avoid rainbow gradients.

---

## 7. Technical / build notes (these matter — agents are the primary channel)

- **Static, fast, semantic, responsive, accessible.** Real HTML headings/markup (agents and crawlers read structure).
- **GEO must-haves** (this is how agents discover and recommend jerrycan):
  - `llms.txt` + `llms-full.txt` at the site root, with descriptive link text and a quickstart-first ordering.
  - `SoftwareApplication` + `Organization` schema.org JSON-LD.
  - The **comparison page (§5.6) as a real, indexable route** with a feature table and a visible last-updated date — this is the single highest-value asset for "best Rust REST framework" answers.
  - The **byte-identical** headline/tagline string in `<title>`, OG tags, and meta description.
  - An **OG/social image** rendering the headline.
- **CTA = adoption, not sales:** GitHub + `cargo install`, never "request a demo."

---

## 8. Out of scope / do NOT

- ❌ No buzzword hero ("the agentic platform for modern teams"). The zero-jargon line wins.
- ❌ No test-mechanics anywhere a human reads (see §4 banned list).
- ❌ No signup/email wall, no aggressive tracking.
- ❌ No fabricated benchmarks, no "fastest" claim, **no "built on axum" claim** (jerrycan has its own core on Tokio + hyper).
- ❌ No fake testimonials or logos.

---

## 9. Assets needed from the team

- Wordmark/logo + favicon.
- The unedited hero demo recording.
- Real comparison-table content (and benchmark numbers when available).
- OG/social image.

---

## 10. Open decisions (flagged for sign-off)

- Exact hero punctuation (words locked).
- Color palette + typography.
- Whether to include a live in-browser "try it" element (nice-to-have, not v1).

---

## Update note (2026-07-23) — CTA superseded (onboarding spec §4.6, Rule 7)

The locked "GitHub + `cargo install`" CTA in §5.9 / §7 is **superseded** by the
agent-first onboarding from the onboarding redesign (spec §4.6). The primary CTA
is now the pasteable one-liner —
`Fetch https://jerrycan.cc/start and follow it to set up jerrycan and build my backend.`
— with the shell installer
`curl -fsSL https://jerrycan.cc/install.sh | bash -s -- --agent <id>` as the
run-it-yourself alternative. `cargo binstall` / `cargo install` remain as
secondary "install the CLI directly" paths. This note records the change; the
section copy above is intentionally left intact per the locked-copy rule.
