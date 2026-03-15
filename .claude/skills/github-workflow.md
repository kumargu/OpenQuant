---
name: github-workflow
description: Use this skill for the full GitHub development workflow — from vision to merged PR. Covers issue creation, PR lifecycle, baselines, benchmarks, CI gates, code review polling, auto-fix loops, and comment monitoring. Apply it whenever creating issues, PRs, running benchmarks, monitoring reviews, or responding to feedback.
---

# GitHub Development Workflow

## Purpose

This skill defines the end-to-end GitHub workflow for OpenQuant. It codifies
the patterns we've learned for shipping changes reliably: from initial idea
through issue tracking, implementation, benchmarking, PR creation, CI
validation, code review, and merge.

The key insight: **automation removes human bottlenecks**. Polling loops for
reviews, auto-fixing CI failures, and responding to comments without manual
intervention lets us ship faster while maintaining quality.

---

## When to use

Use this skill whenever you are:

- Planning a new feature or change (vision → issue → PR)
- Creating or updating GitHub issues
- Opening a pull request
- Running benchmarks and updating baselines
- Setting up CI gates or performance checks
- Monitoring PRs for review comments
- Responding to reviewer feedback
- Fixing CI failures
- Polling for external events (reviews, CI status, comments)

---

## 1. Vision → Issue → PR Pipeline

Every change follows this flow:

```
Vision/Idea
  → GitHub Issue (tracks the "what" and "why")
    → Branch (named: feat/, fix/, bench/, chore/)
      → Implementation + Tests + Benchmarks
        → PR (with backtest comparison if signal/risk/strategy)
          → Review polling loop
            → Address feedback → Push → Re-poll
              → CI green + Approved → Merge
```

### Creating issues

```bash
# Feature issue
gh issue create --title "Add ATR-based dynamic stop-loss" \
  --body "## Motivation\n...\n## Acceptance criteria\n..."

# Epic issue (tracks multiple sub-tasks)
gh issue create --title "[Epic] Hot-path instrumentation" \
  --body "## Sub-tasks\n- [ ] Feature metrics\n- [ ] Signal metrics\n..."
```

Keep issues focused — one hypothesis per issue, one issue per PR.

---

## 2. Baselines and Benchmarks

### Running benchmarks

```bash
# Full criterion benchmark suite
cd engine && cargo bench --bench hot_path

# Specific benchmark
cd engine && cargo bench --bench hot_path -- backtest_1k

# Backtest comparison (for signal/risk/strategy changes)
python -m paper_trading.benchmark --compare
```

### Updating baselines

After any performance-relevant change, re-run benchmarks and update:

1. **`engine/crates/core/tests/BASELINE.md`** — Human-readable table of all
   measured baselines and their CI gate thresholds
2. **`bench_gate.rs` inline comments** — Keep the baseline comments in sync
   with measured values
3. **`python -m paper_trading.benchmark --save-baseline`** — For backtest
   result baselines

### Baseline rules

- Always measure on the same hardware (document which machine)
- Run multiple times — use criterion's statistical analysis, not single runs
- Record the median, not the mean (less sensitive to outliers)
- Include date in baseline docs so stale numbers are obvious

---

## 3. CI Performance Gates

Performance gates catch catastrophic regressions without CI flakiness.

### How they work

```yaml
# .github/workflows/test.yml
- name: Performance gate
  run: cargo test --test bench_gate --release -p openquant-core -- --ignored
```

Gate tests use `#[ignore]` so they skip during `cargo test --workspace`
(debug mode) and only run explicitly with `--release -- --ignored`.

### Threshold strategy

```
CI Gate = Baseline × 30-70x
```

Why so generous? CI Ubuntu runners are ~25x slower than local M4 in
debug-like conditions. The gates catch "something got 100x slower"
regressions, not "2% regression". Use `cargo bench` locally for precision.

| Benchmark | Local Baseline | CI Gate | Multiplier |
|---|---|---|---|
| feature_update | 8.9ns | 2µs | ~225x |
| on_bar | 64ns | 5µs | ~78x |
| backtest_1k | 67µs | 5ms | ~75x |
| backtest_10k | 673µs | 50ms | ~74x |

### When gates fail

1. Don't widen the gate — investigate first
2. Run `cargo bench` locally to get precise numbers
3. Check if the regression is real or CI noise
4. If real: fix the performance issue
5. If CI-specific: consider if the threshold needs adjustment

---

## 4. Code Coverage

### Running coverage locally

```bash
# Install cargo-tarpaulin if not present
cargo install cargo-tarpaulin

# Run coverage
cd engine && cargo tarpaulin --workspace --out html

# Quick summary
cd engine && cargo tarpaulin --workspace --out stdout
```

### Coverage rules

- New code should have tests — don't merge untested signal/risk logic
- Coverage is a guide, not a target — 100% coverage with weak assertions
  is worse than 80% coverage with strong property tests
- Focus coverage on: signal evaluation, risk checks, exit logic, config
  parsing, edge cases (NaN, zero volume, empty bars)

---

## 5. PR Creation

### Standard PR

```bash
gh pr create --title "Add ATR-based dynamic stop-loss" --body "$(cat <<'EOF'
## Summary
- Implemented ATR-multiplier stop-loss in exit module
- Configurable via `stop_loss_atr_mult` in openquant.toml
- Falls back to fixed pct stop if ATR mult is 0

## Backtest comparison
| Metric | Before | After | Delta |
|---|---|---|---|
| Total return | 2.3% | 2.8% | +0.5% |
| Max drawdown | -1.2% | -0.9% | +0.3% |
| Sharpe | 1.4 | 1.7 | +0.3 |

## Test plan
- [x] Unit tests for exit logic
- [x] Backtest comparison on BTC/USD 7d
- [x] CI gates pass

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

### PR requirements (from CLAUDE.md)

- Every PR touching signals/risk/strategy **must** include a backtest
  comparison table
- Generate it with: `python -m paper_trading.benchmark --compare`
- One hypothesis per PR — don't bundle unrelated changes

### After creating any PR — MANDATORY

Start the review monitoring loop immediately:

```
/loop 5m Review PR #<N> at <URL> for new review comments. If new comments exist, assess them, fix the code, commit, push, and reply to the comment.
```

This is not optional. Every PR gets a monitor loop.

---

## 6. Code Review Polling Loop

The core automation pattern: poll for review comments, fix issues, respond.

### How it works

```
/loop 5m Review PR #<N> at <URL> for new review comments. \
  If new comments exist, assess them, fix the code, commit, push, \
  and reply to the comment.
```

This creates a cron job that every 5 minutes:

1. Fetches PR review comments via `gh api`
2. Compares timestamps to find new comments
3. For each new comment:
   a. Reads the comment and understands the feedback
   b. Checks out the PR branch
   c. Makes the fix
   d. Runs `cargo fmt` + `cargo clippy` + `cargo test`
   e. Commits and pushes
   f. Replies to the comment explaining the fix

### Checking for new comments

```bash
# Inline review comments (code-level feedback)
gh api repos/OWNER/REPO/pulls/N/comments

# General PR comments (conversation-level)
gh api repos/OWNER/REPO/issues/N/comments

# Review summaries
gh pr view N --json reviews
```

### Responding to comments

```bash
# Reply to an inline review comment
gh api repos/OWNER/REPO/pulls/N/comments/COMMENT_ID/replies \
  -f body="Fixed in <commit>. <explanation>"

# Reply to a general PR comment
gh api repos/OWNER/REPO/issues/N/comments \
  -f body="Addressed — see <commit>."
```

---

## 7. CI Polling Loop and Auto-Fix

When CI fails, don't wait — detect and fix automatically.

### CI status polling

```bash
# Check CI status for a PR
gh pr checks <N>

# Get detailed run logs
gh run view <RUN_ID> --log-failed

# List recent runs for a branch
gh run list --branch <branch> --limit 3
```

### Auto-fix workflow

When a CI failure is detected:

1. **Fetch the failure logs**: `gh run view <ID> --log-failed`
2. **Identify the failure type**:
   - `cargo fmt` failure → run `cd engine && cargo fmt --all`, commit, push
   - `cargo clippy` failure → fix the warning, commit, push
   - Test failure → `cd engine && cargo test`, investigate, fix, commit, push
   - Performance gate failure → `cd engine && cargo bench` locally, investigate
3. **Push the fix** and monitor the new CI run
4. **Repeat** until CI is green

### Combined PR + CI loop

For maximum automation, the review polling loop handles both:
- New review comments → fix and respond
- CI failures → diagnose from logs and fix

The `/loop` prompt can include both:

```
/loop 5m Review PR #N for new comments and check CI status. \
  If new comments: assess, fix, commit, push, reply. \
  If CI failing: fetch logs, diagnose, fix, push.
```

---

## 8. Handling External Comments

### Types of commenters

| Commenter | How to handle |
|---|---|
| Codex bot (`chatgpt-codex-connector`) | Automated review — assess P1/P2 suggestions, fix valid ones |
| Repo owner (`kumargu`) | Human feedback — highest priority, fix immediately |
| Other contributors | Assess merit, fix if valid, discuss if unclear |
| Bots (dependabot, etc.) | Usually informational — act only if actionable |

### Fetching all comment types

```bash
# Inline code review comments (most common for feedback)
gh api repos/OWNER/REPO/pulls/N/comments \
  --jq '.[] | "\(.id) | \(.user.login) | \(.created_at)\n\(.body[:200])\n"'

# General conversation comments
gh api repos/OWNER/REPO/issues/N/comments \
  --jq '.[] | "\(.id) | \(.user.login) | \(.created_at)\n\(.body[:200])\n"'

# Review-level comments (approve/request changes/comment)
gh pr view N --json reviews \
  --jq '.reviews[] | "\(.author.login): \(.state) - \(.body[:200])"'
```

### Detecting new comments (timestamp filtering)

Track the last-seen timestamp and filter:

```bash
# Only comments after a specific time
gh api repos/OWNER/REPO/pulls/N/comments \
  --jq '[.[] | select(.created_at > "2026-03-15T13:46:15Z")]'
```

### Using /pr-comments (built-in)

Claude Code has a built-in `/pr-comments` slash command:

```
/pr-comments           # auto-detects PR for current branch
/pr-comments 30        # specific PR number
/pr-comments <URL>     # specific PR URL
```

This fetches and displays all comments. Useful for quick manual checks
between polling loop cycles.

### Response protocol

1. **Acknowledge** — confirm you understood the feedback
2. **Fix** — make the change on the branch
3. **Verify** — run fmt + clippy + tests locally
4. **Commit** — with a clear message referencing the feedback
5. **Push** — to trigger CI
6. **Reply** — to the specific comment with commit SHA and brief explanation

Example reply format:
> Fixed in `abc1234`. `engine_kwargs()` now excludes Rust-internal fields
> (`min_score`, `estimated_cost_bps`) since they aren't exposed via PyO3.
> Added a test asserting they don't leak into the kwargs dict.

---

## 9. Loop Management

### Starting loops

```bash
# PR review monitor (mandatory for every PR)
/loop 5m Review PR #30 at <URL> for new review comments...

# CI status monitor
/loop 5m Check CI status for PR #30...

# Combined (recommended)
/loop 5m Review PR #30 for comments and CI status. Fix any issues found.
```

### Listing active loops

Use `CronList` to see all active scheduled tasks.

### Stopping loops

Use `CronDelete` with the job ID to cancel a loop. Stop loops when:
- PR is merged
- PR is closed
- User explicitly cancels
- No activity for extended period

### Loop auto-expiry

All recurring tasks auto-expire after 3 days. For long-lived PRs,
you may need to restart the loop.

---

## 10. Full Lifecycle Example

Here's the complete flow for a typical change:

```
1. VISION:   "We need ATR-based stop-losses"
2. ISSUE:    gh issue create --title "Add ATR stop-loss"
3. BRANCH:   git checkout -b feat/atr-stop-loss
4. CODE:     Implement in exit.rs, add to openquant.toml with detailed comments
5. TEST:     cargo test, add unit tests for new logic
6. BENCH:    cargo bench, update BASELINE.md if changed
7. BACKTEST: python -m paper_trading.benchmark --compare
8. COMMIT:   git commit with clear message
9. PR:       gh pr create with backtest comparison table
10. LOOP:    /loop 5m Review PR #N...
11. REVIEW:  Loop auto-detects comments, fixes, pushes, replies
12. CI:      Loop auto-detects failures, diagnoses, fixes, pushes
13. GREEN:   CI passes, reviewer approves
14. MERGE:   gh pr merge (or wait for owner)
15. CLEANUP: CronDelete the loop, update baselines on main
```

---

## Quick Reference

| Task | Command |
|---|---|
| Create issue | `gh issue create --title "..." --body "..."` |
| Create PR | `gh pr create --title "..." --body "..."` |
| Check CI | `gh pr checks N` |
| CI failure logs | `gh run view ID --log-failed` |
| PR review comments | `gh api repos/O/R/pulls/N/comments` |
| PR conversation | `gh api repos/O/R/issues/N/comments` |
| Reply to review | `gh api repos/O/R/pulls/N/comments/ID/replies -f body="..."` |
| Run benchmarks | `cd engine && cargo bench` |
| Run gate tests | `cargo test --test bench_gate --release -p openquant-core -- --ignored` |
| Save baseline | `python -m paper_trading.benchmark --save-baseline` |
| Compare backtest | `python -m paper_trading.benchmark --compare` |
| Start monitor | `/loop 5m Review PR #N...` |
| View comments | `/pr-comments N` |
| List loops | CronList |
| Stop loop | CronDelete with job ID |
