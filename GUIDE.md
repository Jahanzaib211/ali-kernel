# Ali Kernel, Explained For You

_Written 2026-07-02. No programming background assumed. Skim the bold bits if you're short on focus — each section stands alone._

## TL;DR

- **Ali Kernel is a small program you run on your own computer** that sits between AI models (your local model, and Claude Code) and everything that talks to them. It compresses big chunks of text before they get sent anywhere, saving tokens (= money + speed).
- It's written in **Rust**, a programming language chosen because it's fast and hard to crash by accident.
- Today's session did three things: (1) found and fixed real security holes, (2) cleaned up code-quality warnings, (3) wrote this doc.
- Everything is pushed to GitHub once you confirm — nothing leaves your machine until you say so.

---

## Part 1 — What This Project Actually Is

Forget the code for a second. In plain terms:

You have a **local AI model** (Ornith, a 9-billion-parameter model) running on your own GPU. It's free to use because it's yours — no per-token bill. You also use **Claude Code** (me), which costs money per token because it runs on Anthropic's servers.

Ali Kernel is a **middleman program** — a "gateway" — that stands between you and both of these models:

```
You / Claude Code
        │
        ▼
   ALI KERNEL  (this project — runs on your machine, port 9000)
        │
   ┌────┴────┐
   ▼         ▼
Ornith    Claude/Anthropic
(free,    (costs money,
your GPU)  their servers)
```

Every request that flows through it gets **squeezed smaller** first — big chunks of repeated instructions, huge tool outputs, that kind of thing — before being forwarded. Smaller requests = fewer tokens = less money and faster replies. That's the whole point of the "compression" you've been watching in the logs.

It also does other things: it double-checks the model isn't lying about who it is (attestation), lets the model use tools like reading files or running commands (sandboxed so it can't wreck your machine), and remembers past conversations. But the compression is the headline feature right now.

---

## Part 2 — Rust, Explained Without Jargon

**Rust is the programming language this whole project is written in.** A few things worth knowing, because you'll see these words come up constantly:

### "Building" / "Compiling"

Rust code doesn't run directly — it has to be **translated into a program your computer can execute** first. That translation step is called "building" or "compiling." The command for it is:

```bash
cargo build --release
```

If this command finishes without red text, the code is valid Rust and produces a runnable program. If it fails, there's a mistake in the code that has to be fixed before anything can run at all — Rust is strict about this, on purpose (see "Why Rust" below).

### "Cargo" and "Crates"

- **Cargo** is Rust's tool for managing a project — it builds your code, runs tests, and downloads library code other people wrote.
- A **crate** is Rust's word for a "library" — a chunk of pre-written code someone else published that you can plug into your project instead of writing it yourself. This project uses about 25 crates (listed in `Cargo.toml`), things like `axum` (handles web requests) and `tokio` (handles doing multiple things at once).

### "Clippy"

**Clippy is an automatic code critic.** It reads your Rust code and points out things that are technically correct but sloppy, confusing, or slightly wrong — like a very picky editor. Running `cargo clippy` and getting zero complaints is a sign the code is clean, not just "it happens to work."

Today's session: the codebase had 19 clippy complaints. All 19 are now fixed — some were real style problems, a few were dead code left over from half-built features (kept, but clearly labeled "not used yet" instead of silently confusing the linter).

### Why Rust (and why it matters for you)

Rust's whole selling point is: **it refuses to compile code that could crash in certain common, dangerous ways.** Two ways this shows up in THIS project:

1. **Memory safety.** Older languages like C let you accidentally read or write memory you shouldn't, which is how a huge fraction of real-world security vulnerabilities happen. Rust makes that class of bug basically impossible without going out of your way.
2. **The `Result` pattern.** Anything that can fail (reading a file, making a network request) returns a `Result` — either "here's your answer" or "here's what went wrong" — and Rust *forces* the code to deal with the failure case somehow. It can't just get silently ignored the way it can in a lot of other languages. You'll see `?` a lot in the code — that's shorthand for "if this failed, stop here and report the failure upward."

One danger word to know: **`.unwrap()`**. This means "I'm certain this can't fail, so just crash the whole program if it does." It's a shortcut that's fine in a quick test but risky in real production code, because if the assumption turns out to be wrong, the entire program stops instead of handling it gracefully. Part of a code review is checking that `.unwrap()` isn't used somewhere it could actually blow up on real input.

### A few more words you'll see

| Term | Plain-English meaning |
|---|---|
| `struct` | A custom "shape" of data — like a form with named fields (e.g., a `User` struct might have `name` and `email`). |
| `enum` | A fixed list of possible states/options for something (e.g., a traffic light is `Red`, `Yellow`, or `Green` — nothing else). |
| `async` / `await` | Code that can "pause" while waiting on something slow (like a network request) without freezing the whole program. |
| `Option` | Either "there's a value here" (`Some`) or "there's genuinely nothing here" (`None`) — Rust makes you handle the "nothing" case explicitly instead of it silently causing a crash later. |
| Test / `cargo test` | Small scripts that check specific pieces of the code behave correctly. If you change something and a test starts failing, that's an early warning you broke something. |

---

## Part 3 — The Project's Shape

Two separate programs live in this one repo:

- **`ali-kernel`** — the gateway itself. Runs quietly in the background, listens on port 9000, does all the actual work.
- **`ali-tui`** — the terminal interface you actually look at and type into when you run `./ali`. It's a "client" — it talks to the gateway, it doesn't do the heavy lifting itself.

The gateway processes every request through a numbered pipeline (this is genuinely how the code is organized, file by file):

| Stage | What it does | Plain English |
|---|---|---|
| L0–L2 | Config + security checks | "Am I even set up correctly, and has anyone tampered with my files?" |
| L3 | Routing | "Is this a quick hello, or something that needs the full model?" |
| L4 | Memory | "Have we talked about this before?" |
| L5 | Context assembly | "Build the actual prompt to send to the model." |
| **L5.5** | **Compression** | **"Shrink anything oversized before it goes further."** ← the part you've been watching |
| L6 | Inference | "Actually ask the model the question." |
| L7 | Tools | "If the model wants to read a file or run a command, do that safely." |
| L8 | Critique | "Have the model double-check its own answer." |
| L9 | Logging | "Write down what just happened." |

Everything you've seen in `logs/gateway.log` — the "PIPE L5.5 compressed: X → Y chars" lines — is stage L5.5 reporting what it just did.

---

## Part 4 — What Got Hardened Today (and why it mattered)

You asked for a security review. Here's what that turned up, explained without jargon:

### 1. The model could read or write *any file on your computer*

The tools that let the model read/write files (`read_file`, `write_file`) had **no boundary** — if the model was asked (or tricked) to write to, say, your SSH keys or `.bashrc`, it just... would. There was a config setting that looked like it should prevent this (`allowed_workspace_paths`) but it was never actually wired up — it existed in name only.

**Fixed:** file access is now boxed in to specific folders (the project folder, the dev workspace, the app's own data folder). Anything outside that gets rejected. Verified with automated tests that check both "normal access still works" and "trying to escape the box still fails."

### 2. Any webpage you visited could have silently used your local AI setup

This is the scarier one. The gateway was configured to accept requests from *any website's background code*, not just from your own tools. In practice, that means: if you had the gateway running and visited a malicious or compromised webpage, that page's own JavaScript could have quietly sent commands to your gateway — including the file-write bug above — with **zero interaction from you** beyond having the tab open.

**Fixed:** removed the setting (`CorsLayer::permissive()`, a "let any website talk to me" switch) that made this possible. There was never a real reason for a website to need that access.

### 3. Smaller things

- The network-exposure bug from earlier this session (`llama-server` listening on all network interfaces instead of just your machine) — already fixed, but I found it was reintroduced in a couple of leftover default settings and closed those too, so it can't quietly come back.
- A flaky test that would occasionally fail for reasons that had nothing to do with real bugs (multiple tests were sharing one temp file behind the scenes) — fixed so the test suite is reliably trustworthy.

**Bottom line:** before today, if you'd left the gateway running and visited the wrong webpage, you could have had files silently added to or modified on your own computer. That's now closed.

---

## Part 5 — Where Things Live (cheat sheet)

```
ali-kernel/          the gateway program's source code
  src/tools.rs         the file-read/write/bash tools (and their new safety boundary)
  src/main.rs           wires everything together, starts the web server
  src/anthropic_proxy.rs  the new "compress Claude Code's own traffic" feature
ali-tui/             the terminal interface source code
ali                  the script you actually run (`./ali`)
logs/gateway.log     the live activity log — this is where "proof" always comes from
COMPRESSION.md       plain-English guide to just the compression feature
GUIDE.md             this file
```

To check things are healthy at a glance:

```bash
curl http://127.0.0.1:9000/health          # is the gateway up?
cargo test --workspace                     # do all the automated checks pass?
cargo clippy --all-targets -- -D warnings  # any code-quality complaints?
```

If all three come back clean, the project is in a good state.

---

## Part 6 — Panic page / quick reference

_The commands you keep copy-pasting, all in one place. If something is broken, scroll to "If X then Y" first._

### Restart everything (the most common move)

```bash
# 1. Kill the TUI (it doesn't auto-reconnect — it gets wedged when the gateway dies)
pkill -f "target/release/ali-tui"

# 2. Bounce the gateway service
sudo systemctl restart ali-kernel

# 3. Bring the TUI back
nohup ~/ali-kernel/target/release/ali-tui >/dev/null 2>&1 &
disown
```

You should see the gateway come back within a second or two. If it doesn't, jump to "If X then Y" below.

### After a rebuild (cargo build --release)

The gateway seals its own hash on first run (`/tmp/ali-ram-moat/`). A rebuild changes the binary, so the next start refuses with `FATAL: Binary tampered!`. Always do this sequence after rebuilding:

```bash
cd ~/ali-kernel
cargo build --release
rm -rf /tmp/ali-ram-moat          # let the new binary reseal on next start
sudo systemctl restart ali-kernel
~/ali-kernel/target/release/ali-tui &   # relaunch TUI if you had it open
```

**Don't** skip the `rm -rf`. **Don't** expect a running gateway to pick up a new binary on its own — it has to actually restart.

### Is it actually working?

```bash
curl -s http://127.0.0.1:9000/health          # gateway alive?
systemctl status ali-kernel --no-pager       # service state + last few errors
pgrep -af "ali-kernel|ali-tui|llama-server"  # what's actually running, with PIDs
```

A healthy gateway returns JSON like `{"status":"alive","version":"2.1.0","backend":"healthy",...}`.

### Watch compression live

```bash
# Just the squeeze lines (what you actually want to see)
tail -F ~/ali-kernel/logs/gateway.log | grep --line-buffered "L5.5 compressed"

# Everything (full pipeline trace — noisier)
tail -F ~/ali-kernel/logs/gateway.log
```

Each line is `before → after (-X%) [which-path]`. Example:
```
PIPE L5.5 compressed: 50289 → 12373 chars (-76%) [claude-proxy]
```
That `-76%` is what saved you tokens.

> **Heads up:** the TUI uses `/v1/chat/completions` (OpenAI format), which only fires the older
> "big text → small text" compression. The **new** paths (system-as-array, tool_result blocks)
> only fire on `/v1/messages` traffic — i.e. **Claude Code sessions in this repo**. Watch the
> log while you use Claude Code here, not while using the TUI.

### How much have we saved?

```bash
# All-time squeeze count
grep -c "L5.5 compressed" ~/ali-kernel/logs/gateway.log

# Last 20 squeezes with their savings
grep "L5.5 compressed" ~/ali-kernel/logs/gateway.log | tail -20

# Today's only
grep "$(date +%Y-%m-%d)" ~/ali-kernel/logs/gateway.log | grep "L5.5 compressed"
```

### If X then Y (the part to scroll to when something is broken)

| Symptom | Fix |
|---|---|
| Gateway won't start, journal says `Binary tampered!` | `rm -rf /tmp/ali-ram-moat && sudo systemctl restart ali-kernel` |
| `curl 127.0.0.1:9000/health` → Connection refused | `systemctl status ali-kernel` — if it's "activating (auto-restart)" in a loop, see the row above |
| TUI shows "error sending request to URL" | `pkill -f target/release/ali-tui` then relaunch — it doesn't auto-reconnect |
| Gateway up but replies are wrong / empty | Check `llama-server` is alive: `systemctl status llama-server`, or `curl 127.0.0.1:8081/v1/models` |
| `sudo systemctl` asks for password | The previous sudo session expired. Enter it once; lasts 15 min by default. |
| Nothing in the log | Check you're tailing the right file: `ls -la ~/ali-kernel/logs/gateway.log` |
| "Permission denied" running the binary by hand | Run it from `~/ali-kernel/` (the systemd unit sets `WorkingDirectory=`; your shell doesn't). Or use `sudo systemctl restart ali-kernel` instead. |

### Where things live

| Thing | Path |
|---|---|
| Gateway log (live) | `~/ali-kernel/logs/gateway.log` |
| Model log (live) | `~/ali-kernel/logs/llama-server.log` |
| Gateway binary | `~/ali-kernel/target/release/ali-kernel` |
| TUI binary | `~/ali-kernel/target/release/ali-tui` |
| Service unit | `/etc/systemd/system/ali-kernel.service` |
| Attestation moat | `/tmp/ali-ram-moat/` (delete after rebuilds) |
| Env vars (`ANTHROPIC_BASE_URL`, etc.) | `~/.bashrc` |
| Capture data (SFT/DPO) | `~/ali-kernel/training/` |

### Service commands (memorize this row)

```bash
sudo systemctl status ali-kernel      # what's it doing + last log lines
sudo systemctl restart ali-kernel     # bounce it
sudo journalctl -u ali-kernel -n 50   # last 50 journal lines (no sudo needed if you're in `adm` group)
sudo journalctl -u ali-kernel -f      # follow live, like tail -f
```

### Nuclear option (everything wedged)

```bash
pkill -9 -f "target/release/(ali-kernel|ali-tui)"
sudo systemctl restart ali-kernel llama-server
sleep 2
nohup ~/ali-kernel/target/release/ali-tui >/dev/null 2>&1 &
disown
curl -s http://127.0.0.1:9000/health
```

If `health` still fails after this, paste the output of `sudo journalctl -u ali-kernel -n 30` — that's the thing that says *why*.
