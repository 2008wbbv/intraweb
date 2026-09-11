# intraweb

**your neighborhood web**

A local-first intranet that needs no internet, no servers, and no setup. Put the
binary on a laptop or a Raspberry Pi, connect to the same Wi-Fi as everyone else,
and you can see each other, visit each other's pages, and send each other mail.

```sh
intraweb up
```

That is the whole setup. Nodes find each other in about a second.

## The idea

Most "local network" software makes one machine the server and everybody else a
client. Then the server owns your account, your files, and your name, and when
it goes away, so do you.

intraweb does it the other way round:

- **You are a keypair and a folder.** Your identity is an Ed25519 key in your
  vault. Copy the vault to another machine and you are still you.
- **Hubs are places, not owners.** A hub keeps a roster and offers a memorable
  address. It stores none of your content and cannot create or revoke you.
- **One folder, many neighborhoods.** Join a hub at home, another at the
  makerspace, a third at a field site. All three serve the same vault. Nothing
  is copied, and leaving takes nothing away.

The closest familiar comparison is Mastodon's federation, minus the part where
your data lives on somebody's instance.

## Install

Single static binary, no runtime dependencies:

```sh
curl -fsSL https://raw.githubusercontent.com/2008wbbv/intraweb/main/install.sh | sh
```

Or build it yourself:

```sh
cargo build --release   # target/release/intraweb
```

For off-grid provisioning, copying the binary over with `scp` or a USB stick
works exactly as well — there is nothing else to install.

## Use

```sh
intraweb up                        # join the neighborhood
intraweb up --hub --hub-name oak   # host a hub at intranet.local
intraweb up --tui                  # same thing, in the terminal
intraweb doctor                    # why can't I see anyone?
intraweb id                        # my fingerprint, for verifying in person
```

Open the dashboard at `http://intranet.local` (or `http://localhost:8420` if no
hub is running). It is plain HTML and JavaScript, and works on any phone on the
network.

### Your vault

```
~/.intraweb/
  identity.key   secret   your key — back this up, it is your name everywhere
  config.toml    private  nickname and preferences
  intraweb.db    private  who you have met, which hubs you have visited
  site/          PUBLIC   your mini-site, served at /~yourname
  files/         PUBLIC   media and downloads neighbors can pull
```

Drop an `index.html` in `site/` and it is live at every hub you join. Point
`$INTRAWEB_VAULT` somewhere else to run more than one node on one machine.

## Trust

There are no passwords, and no certificate authority — neither would mean much
on an open LAN with no internet.

Instead, every announcement is signed. A node **cannot** announce itself under
someone else's key; the signature would not verify. What a node *can* do is pick
any nickname it likes, because nicknames are labels, not identity.

So intraweb watches the one thing that actually signals an impostor: a name you
have seen before turning up on a key you have not. That gets flagged loudly in
the dashboard rather than quietly rendered as a familiar face. To be certain
about someone, compare fingerprints out loud:

```
$ intraweb id
fingerprint  ba10-4253-4e1b-194a
```

Then mark them verified (`v` in the terminal UI, or the button in the dashboard).

## When nobody shows up

Usually the network, not the software. Plenty of consumer access points isolate
wireless clients or drop multicast, and both look identical from the dashboard:
an empty roster.

```sh
intraweb doctor
```

It listens on both discovery paths and tells you which one is working. intraweb
announces over **mDNS** and **UDP broadcast** at the same time precisely because
so many networks quietly break one of them.

## Status

P0 is done: discovery, identity, the roster, both dashboards, and diagnostics.
Mail, mini-site publishing from the dashboard, and direct file transfer are
next — see `NOTES.md` for the plan and the open questions.

## License

MIT OR Apache-2.0
