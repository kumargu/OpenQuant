# OpenQuant Coding Harness

This repo is a trading system, not a sandbox. Changes must move OpenQuant toward durable autonomous profitability with a single, observable engine path across replay, paper, and live.

## Mission

OpenQuant should make money without hand-rolled daily intervention. The target is serious paper performance on `$10k` capital, with a practical goal of `$100-$200` on strong days and controlled losses on weak days. Losing money is not a reason to explain it away; it is a reason to research, measure, fix, or replace the strategy.

Agents must work from evidence. Every strategy change needs replay results, baseline comparison, code review, and logs that explain what happened.

Token discipline matters. Replays are frequent and verbose, so agents must preserve evidence on disk and surface only compact summaries in-chat.

## Autonomous Operating Contract

An agent owns the task until it is closed. Do not stop at analysis, a partial patch, or a promising replay. Keep working in a loop: understand, implement, measure, debug, review, and either ship or document why the task should be rejected.

The loop closes only when one of these is true:

- A strategy or epic task shows money-making improvement over the agreed baseline across the required replay windows, with no obvious lookahead, leverage, or state bug.
- A bugfix proves the defect is fixed with a focused regression test, relevant replay when behavior could affect trading, and logs or metrics that make the fix observable.
- An operations/logging/refactor task passes tests and demonstrably improves observability, safety, or single-path execution without changing trading behavior unexpectedly.
- Research rejects the idea with a clear experiment, reproducible evidence, and a better next task posted to the issue.

For any task tied to profitability, "works on one lucky month" is not closed. Mark an epic successful only after a larger replay window validates it, normally current YTD plus at least two prior quarters or the windows specified in the issue. Small known bugfixes and logging work do not need full-quarter replay unless they change decisions, sizing, orders, state, or broker reconciliation.

## Non-Negotiables

- `main.rs` is the operator entrypoint. Do not create side entrypoints for trading decisions.
- Replay, paper, and live must share the same runner path as much as possible. Differences must be broker/source/clock only.
- No direct Alpaca trading outside the runner.
- Default behavior must be stable and explicit. New strategy modes start disabled unless intentionally promoted.
- Real-money `live` requires stricter gates than paper. Do not widen live behavior casually.
- Keep `autoresearch/` intact. It is a separate research loop and should not be rewritten by harness work.
- Prefer Rust for trading logic. Python may orchestrate research, but it must not become a second trading engine.
- Delete dead or misleading code. Do not keep stale alternate implementations.

## Work Loop

1. Start from `main` unless the user gives a branch.
2. Read the current code path before editing.
3. Identify the strategy, risk, or operations goal.
4. Find the relevant GitHub issue or epic. If none exists, create one before broad work.
5. Choose the next concrete task from the epic and finish it end to end.
6. Make the smallest coherent code change.
7. Run focused tests, then broader tests if risk is high.
8. Run replay baselines for strategy-impacting work.
9. Save replay stdout/stderr to evidence files, then extract only summary metrics into the issue/PR/chat response.
10. If money-making metrics degrade, inspect logs and state transitions before changing more code.
11. Review your own diff as if it can lose money.
12. Commit with an audit-quality message, open a PR, monitor review, fix comments, and merge only when checks pass.
13. Post the closure evidence back to the issue: tests, replay windows, metrics, failures found, and the next task if the epic continues.
14. After an epic is complete and evidence has been preserved, clean local leftovers without deleting credentials.

## Baseline Discipline

For changes touching signals, sizing, state, regime, execution, or persistence:

- Run a baseline replay and the candidate replay on the same windows.
- Report cumulative return, Sharpe, max drawdown, active days, turnover/orders, and major mode switches.
- Do not stream replay logs into chat. Redirect them to files under `data/replay/...` and report only compact summaries.
- Preserve a known baseline in `BASELINES.md` when a result becomes a reference point.
- If results improve only by adding hidden leverage, concentration, or stale/future data, reject the change.
- If a result looks too good, assume bug until checked against logs and state transitions.
- For epic success, prefer broad validation: current YTD, two earlier quarters, and the known stress window that motivated the work.
- For small non-strategy fixes, record why unit/integration tests are sufficient instead of replay.

Recommended replay pattern:

```bash
scripts/run_basket_replay_quiet.sh 2026_ytd_baseline 2026-01-02 2026-04-30
scripts/run_basket_replay_quiet.sh 2026_ytd_classifier 2026-01-02 2026-04-30 \
  --leadership-overlay-sectors faang,chips \
  --leadership-mode replace-with-long-only \
  --leadership-ret5d-threshold 0.02 \
  --leadership-breadth5d-threshold 0.56 \
  --leadership-long-only-leverage 4.0
```

The wrapper should print only:

- replay name and window
- report path and log path
- cumulative return, Sharpe, max drawdown, trading days
- order/turnover summary
- overlay switch counts when applicable
- first failure line only if the replay fails

Never paste thousands of order logs into chat when the same evidence exists on disk.

Current basket reference on merged `main`:

| Window | Mode | Cum Return | Sharpe | Max DD |
|---|---:|---:|---:|---:|
| 2026 YTD | basket baseline | +3.36% | 0.47 | 21.5% |
| 2026 YTD | basket + `faang,chips` overlay | +76.53% | 2.54 | 30.6% |

## Research Standard

Research should answer a decision question, not produce endless charts.

Good research has:

- a hypothesis
- a frozen comparison matrix
- out-of-sample windows
- a baseline
- explicit failure cases
- a recommendation to ship, revise, or reject

Use `autoresearch/` as designed. Do not replace it. For exploratory scripts outside autoresearch, keep them temporary, document outputs, and do not let them become production paths.

## Cleanup Standard

End each completed epic from a clean, reproducible tree.

- Never delete `.env` or `.env.*` at any directory level. These files contain local credentials and must survive branch switches, cleanup, and `git clean`.
- If tracked files were deleted as local cleanup noise, restore them from `main` unless the deletion is part of the intentional PR.
- Review `git status --short` before cleanup. Do not remove untracked source, tests, docs, config, or evidence that should be committed.
- Prefer path-scoped cleanup for known artifact directories. Use repo-wide cleanup only when the preview is small and clearly disposable.
- Remove stale research outputs, replay artifacts, disposable logs, and scratch files only after their useful evidence has been copied into the issue, PR, or baseline notes. Do not delete durable paper/live journal evidence unless it has been intentionally archived or summarized.
- Do not run raw `git clean -fdx`. Ignored files may include credentials, local virtualenvs, caches, and operator-only state.
- Before destructive cleanup of untracked files, preview it with credential exclusions:

```bash
git status --short
git clean -nd -- . ':!.env' ':!.env.*' ':!**/.env' ':!**/.env.*'
```

- When the preview contains only disposable leftovers, clean untracked files with the same exclusions:

```bash
git clean -fd -- . ':!.env' ':!.env.*' ':!**/.env' ':!**/.env.*'
```

- To clean ignored logs, replay output, or cache files, preview ignored-only cleanup first:

```bash
git clean -ndX -- . ':!.env' ':!.env.*' ':!**/.env' ':!**/.env.*'
```

- Then run ignored-only cleanup only if the preview is disposable:

```bash
git clean -fdX -- . ':!.env' ':!.env.*' ':!**/.env' ':!**/.env.*'
```

- For tracked deletions or modifications that are not part of the task, restore only those paths explicitly:

```bash
git restore path/to/file
```

- Finish by confirming:

```bash
git status --short
```

## Code Standards

- Keep decisions in one code path. Avoid replay-only logic unless it is explicitly test instrumentation.
- Make state restart-deterministic. A restart should not change tomorrow's orders.
- Persist enough state to explain and resume decisions.
- Validate inputs at startup: symbols, sectors, thresholds, leverage, paths, and malformed config.
- Guard math at boundaries: finite prices, positive prices, no NaN propagation.
- Use structured `tracing` logs for decisions, state transitions, sizing, orders, reconciliations, and rejects.
- Add config only when an operator can understand and use it.
- Prefer named structs and enums over magic strings.
- Tests must include the rejection path, not only the happy path.

## Logging Standard

Important trading paths must log enough to reconstruct:

- what strategy mode was active
- what data snapshot was used
- why the decision fired
- what target book was created
- what orders were sent
- what broker positions were observed
- whether reconciliation matched target
- what state was persisted

Every new or materially changed implementation path must include a complete
operator log surface:

- `info`: normal lifecycle, selected config, state source, decision summaries,
  sizing summaries, orders, reconciliation, and persistence.
- `warn`: recoverable degraded behavior, fallback paths, ignored stale inputs,
  skipped optional work, or conditions requiring operator attention.
- `error`: fatal startup/runtime failures, broker/API failures, rejected orders,
  missing required data, or anything that stops safe execution.
- `bug!`: should-not-happen internal states, invariant breaks, mismatched state,
  suspicious fallback behavior, target/account divergence, or unexpected
  sidecar/config interactions. A `bug!` must be visible in logs with
  `bug=true bug_marker="BUG"` and counted by the metrics recorder when the
  runner is active.

Use `RUST_LOG` to switch detail levels. Paper runs should write durable logs to `data/journal/engine.log` or the configured journal path. High log volume is acceptable for now; missing decision evidence is not.

For replay evidence, high log volume belongs in files, not in the chat transcript.

## PR Standard

Every PR that affects trading behavior must include:

- what changed
- what stayed default-off
- replay windows and metrics
- tests run
- safety boundaries for paper/live
- known risks

No final response for strategy-impacting work is complete unless it explicitly says one of:

- replay baseline and candidate were run, with windows and metrics
- replay was not run, with a concrete reason and why that is acceptable for this task

Do not merge with failing required checks. If GitHub state is confused, diagnose it rather than bypassing checks.

## Issue / Epic Standard

For epic work:

- pick one issue
- finish it
- post results back to the issue
- open a focused PR
- after merge, pick the next issue

Do not drift into unrelated refactors while chasing a trading result. If a new bug appears, file it or explicitly pull it into scope.

Each issue update should make the next agent self-sufficient:

- current hypothesis
- exact commands or replay windows used
- baseline metrics and candidate metrics
- code paths touched
- whether the task is closed or what remains

## Success Bar

A change is not successful because it compiles. It is successful when:

- replay says it improves the right metric across the required windows, or tests prove a contained non-strategy fix
- paper behavior is observable and restart-safe
- the code path is simpler or no more complex than necessary
- the logs explain the decision
- future agents can maintain it without oral history
