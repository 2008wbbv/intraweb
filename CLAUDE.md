# CLAUDE.md — intraweb

Architecture rules and project context. Read this before changing anything;
it exists so sessions do not have to re-derive decisions from chat history.

## What this is

A zero-config local intranet. Slogan: **your neighborhood web**. One static
binary, no internet, no central server.

## The load-bearing idea

**A person is a keypair and a folder. A hub is a place to meet.**

Every design question resolves against that sentence. In particular:

- A hub **never** stores, proxies, or relays another node's content. The roster
  hands out addresses; the browser goes straight to the peer. If a change would
  make the hub a relay, the change is wrong.
- A node can be on **many hubs at once**, serving the same vault to all of them.
  Anything that assumes a single "home" hub is wrong.
- The vault is portable. Copy it to another machine and you are the same person,
  on every hub, with no migration step.

## Crate layout

| Crate | Owns | Must not |
| --- | --- | --- |
| `intraweb-core` | Identity, vault, config, SQLite keyring, peer types | Know anything about networks |
| `intraweb-net` | mDNS, UDP beacon, roster, diagnostics, orchestration | Know anything about HTTP or the UI |
| `intraweb-cli` | CLI, JSON API, web dashboard, terminal UI, HTTP probing | Hold state the roster should own |

The split is what lets discovery be tested without touching identity, and the
roster be rendered twice without duplicating logic.

## Rules that are easy to get wrong

1. **Verify before you record.** Anything arriving from the network is hostile
   until its signature checks out. `absorb()` in `net/node.rs` is the only path
   from wire to keyring; keep it that way.
2. **Drop self-echo before the database, not after.** Our own broadcast returns
   to us and verifies correctly, because it really is our signature. It must be
   discarded at the top of `absorb()`. (This was a real bug; there is a
   regression test.)
3. **Nicknames are labels, keys are identity.** Never enforce nickname
   uniqueness — there is no registry and there cannot be one. Detect and *show*
   name/key conflicts instead. `TrustState::NicknameConflict` must never be
   silently downgraded once raised.
4. **Signed fields are scrubbed before signing.** `presence::scrub` strips the
   `|` separator so a hostile nickname cannot smuggle extra fields into the
   canonical encoding. Never sign unscrubbed user input.
5. **An explicit choice always wins.** A hub prefers port 80 for a clean URL,
   but a chosen port is honored. Silently relocating the dashboard sends people
   chasing a dead address. (Also a real bug, also a regression test.)
6. **Both discovery paths, always.** mDNS alone fails on networks that filter
   multicast; broadcast alone gives up `.local` names. Neither is optional.
7. **Never hold the store lock across the roster lock**, or across an `.await`.
   `absorb()` scopes them deliberately.
8. **Looking is not joining.** `surf` and anything else that only inspects the
   network uses `Node::observe`, which listens without announcing. Browsing
   what neighbors publish must not change what they see, and a second
   announcement under the same key would collide with a node already running on
   the machine.
9. **Any listing of people shows trust.** `surf` prints the conflict warning and
   fingerprint next to a name whose key has changed. Surfing is exactly where
   impersonation would pay off, so no surface may render a bare nickname as if
   it were an identity.
10. **A rendered block must fit what it draws.** The TUI header was sized four
    rows while drawing three lines plus borders, silently clipping the roster
    counts. `HEADER_LINES`/`HEADER_HEIGHT` encode the relationship and a test
    asserts it.
11. **Tests never assume an empty network.** The machine running them may have
    real nodes on it. Assert invariants (a warning always carries a remedy; the
    verdict follows from the counts), not specific peer counts.

## Conventions

- Comments explain *why*, especially where a line encodes a security decision
  or a network-reality workaround. Do not narrate what the code plainly says.
- Test names are sentences describing the behavior being protected
  (`a_new_key_wearing_a_known_name_is_flagged`).
- Every bug found in real testing gets a regression test naming it as such.
- Error messages address the operator and say what to do, not what failed
  internally. `doctor` is the model for this.
- No `unwrap()` outside tests.

## Testing

```sh
cargo test                 # unit + regression
cargo clippy --all-targets -- -D warnings
```

Screenshots of the terminal UI are taken from the real binary, not mocked:

```sh
tmux new-session -d -s iw -x 84 -y 13 "INTRAWEB_VAULT=/tmp/c intraweb up --tui"
sleep 4 && tmux capture-pane -p -t iw
```

Beware `pkill -f intraweb` when cleaning up: the pattern matches the shell
running it and kills your own command. Use `pkill -x intraweb`.

Two nodes on one machine, which is also how discovery is tested end to end:

```sh
INTRAWEB_VAULT=/tmp/a intraweb up --hub --nickname alice --port 8480 &
INTRAWEB_VAULT=/tmp/b intraweb up --nickname bob --port 8481 &
curl -s localhost:8480/api/peers
```

The beacon sets `SO_REUSEPORT`, so both nodes share the beacon port; give them
distinct API ports.

## Deliberately out of scope

Multi-hop mesh routing across subnets, BLE fallback, database replication
between nodes, dynamic server-side scripts, global DNS, and 32-bit ARM
(Pi Zero) targets. Say no to these rather than half-building them.
