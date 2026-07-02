# Ali Kernel — Project Instructions

A single-binary Rust gateway (`ali-kernel/`) that wraps a local llama.cpp model
(Ornith-1.0-9B) behind an OpenAI-compatible API, plus an Anthropic `/v1/messages`
proxy face so Claude Code's own traffic can route through it too. `ali-tui/` is
the terminal client. Both compression paths share `compress.rs`.

Read `GUIDE.md` first if you need the plain-English version — the owner isn't a
programmer, has ADHD, and that doc's tone (short sections, bold key lines, no
dense paragraphs) is the template for anything written for them directly.

## Doc map — don't duplicate, update in place

- `README.md` — reference/install docs. Keep API examples in sync with actual
  handler output (this drifted badly once already — see git log 2f800aa).
- `STATUS.md` — live status, dated entries per session. Add a new dated section
  at the top each session; don't rewrite old ones.
- `ROADMAP.md` — phase tracker + curated crate list.
- `BUILD.md` — who builds this and how (parties/roles/method).
- `COMPRESSION.md` / `GUIDE.md` — owner-facing explainers.

These four/five docs already overlap more than they should. Don't add a sixth
tracking doc — fold new status into the existing ones.

## Operational gotchas (will bite you if forgotten)

- **Binary + constitution attestation**: the gateway hashes itself and
  `constitutions/SOUL.md` on first run, sealed in `/tmp/ali-ram-moat/`. Any
  rebuild changes the binary hash, so the *next start* fails with "Binary
  tampered!" until that directory is cleared. After every `cargo build
  --release`, if the gateway is systemd-managed: `rm -rf /tmp/ali-ram-moat &&
  sudo systemctl restart ali-kernel`. Don't just clear the hash and expect a
  running process to pick up the new binary — it won't until it actually
  restarts (verified this the hard way in the 2026-07-02 session).
- **Editing `constitutions/*.md`** trips the same attestation mechanism (they're
  hashed too). Treat them as more sensitive than ordinary docs.
- **Both services are systemd-managed** (`llama-server.service`,
  `ali-kernel.service`, installed via `sudo bash install-service.sh`) —
  auto-start on boot. Don't reach for manual `nohup ... &` first; check
  `systemctl status llama-server ali-kernel` before assuming nothing is running.
- **Never bind anything to `0.0.0.0`.** This has been the single most-repeated
  finding across sessions (llama-server flags, the `ali` launcher script, and
  two vestigial config defaults all did this at different points). Loopback
  (`127.0.0.1`) only, always. Same goes for permissive CORS — there's no
  legitimate browser client, don't reintroduce it.
- File tools (`read_file`/`write_file` in `tools.rs`) are confined via
  `AliConfig::resolve_tool_path()` to `allowed_workspace_paths`. If you add a
  new file-touching tool, route it through that, don't call `std::fs` directly.

## Current known bottleneck (as of 2026-07-02)

The project's actual thesis — the local model critiquing and improving itself
— is still blocked: self-critique is too lenient to produce real DPO
preference pairs (`critique.rs`/`evals.rs`). Recent sessions have added real
value elsewhere (Anthropic proxy, security hardening, doc cleanup) but haven't
touched this. Worth surfacing if asked "what's next" — new feature surface
area is accumulating faster than the core self-improvement loop is proving out.

## Git conventions

Solo-owner repo (`github.com/Jahanzaib211/ali-kernel`, renamed from
`ornith-sovereign-gateway` in 2026-07-02) — direct-to-`master` commits are the
norm, no PR flow. Still confirm with the owner before every `git push`, even
though it's their own repo — this has come up repeatedly and they've always
said yes, but the confirmation step itself is expected.

Before committing: `cargo build --release`, `cargo clippy --all-targets --
-D warnings`, `cargo fmt --check`, `cargo test --workspace` should all be clean.
They were all fully green as of commit `2f800aa` — keep them that way rather
than letting warnings accumulate again.
