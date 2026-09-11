# NOTES.md — intraweb

Running log of decisions, open questions, and known gaps. Append; do not rewrite.

## Roadmap

| Phase | Scope | State |
| --- | --- | --- |
| P0 | Discovery, identity, roster, both dashboards, doctor | **done** |
| P1 | Local mail, store-and-forward, signed | not started |
| P2 | Mini-site publishing from the dashboard | not started |
| P3 | Chunked, resumable direct file transfer | not started |
| P4 | Mail attachments (P1 ∪ P3) | not started |

The `mail` table already exists in schema v1, so P1 is logic only.

<!-- MEMORY_UPDATE_START -->
### Update: 2026-09-11
* **Decisions/Changes**: Pre-build architecture settled and P0 built. Rust
  workspace (`core` / `net` / `cli`), edition 2024. Identity is Ed25519 with
  trust-on-first-use; seeded from `getrandom` into `SigningKey::from_bytes`
  rather than taking an RNG parameter, so the crate is not coupled to whichever
  `rand_core` major `ed25519-dalek` currently tracks (3.0 wants 0.10).
  Federation model revised mid-session per Ben: hubs own nothing, a node joins
  many hubs at once and serves one portable vault to all of them — Mastodon's
  shape without the data gravity. This killed the earlier `ben@basecamp`
  node-scoped addressing; identity is the pubkey, nickname is decoration.
  Presence is signed and travels two ways (mDNS TXT + UDP broadcast beacon),
  with a hand-built canonical encoding because a signature is only worth
  something if both sides agree byte-for-byte. Dashboard is web + ratatui, both
  reading one shared `Roster` object rather than the TUI doing HTTP to itself.
  `doctor` ships in P0 as argued: it separates "nobody here" from "your AP is
  isolating clients", which otherwise look identical.
* **Known Bugs/Gaps**: Two real bugs found by end-to-end testing, both fixed
  with named regression tests: (1) a node recorded its own echoed beacon in its
  own keyring, because self-echo was dropped at the registry rather than before
  the database write; (2) `--port` was silently swallowed by the hub's port-80
  preference, leaving the operator curling a dead port. Still open: TUI is
  local-only (a remote TUI would need an HTTP client, deferred rather than pull
  in `reqwest` against the lean-binary goal); no release CI or cross-compile
  matrix yet, so `install.sh` points at releases that do not exist until that
  lands; transport port 8421 is announced but nothing listens on it until P1;
  beacon replay is bounded only by a 300s clock-skew window, which is
  proportionate for LAN presence but is not a nonce.
<!-- MEMORY_UPDATE_END -->

## Open questions for Ben

Answered in session (recorded so they are not re-litigated):

1. Topology — hub + peers, hub is a meeting point only.
2. Trust — Ed25519 + TOFU, no passwords, no CA.
3. Dashboard — both web and TUI, over one source of truth.
4. Targets — aarch64 + x86_64; Pi Zero / ARMv6 dropped.
5. Federation — many hubs per node, one shared vault. *(revised mid-build)*

Still unanswered, defaults assumed and noted in the build:

6. **Resumable transfers in P3** — assumed yes. Worth ~100 LOC for field use.
7. **Release hosting** — assumed GitHub Releases; no domain wired up.
8. **Service lifecycle** — no systemd/launchd integration yet. The port-80
   fallback now prints a real `setcap` command instead of naming an unbuilt
   subcommand. Decide in P1 whether `intraweb service install` is worth having.
9. **Mail addressing** — now that identity is portable, is mail addressed to a
   fingerprint, or to a nickname resolved through whichever hub you share?
   Portability makes this a genuinely open question, not a default.
