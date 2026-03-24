# Debugging Learnings — Log-Driven Strategy Development

How we went from -$5,164 to +$2,407 through structured log analysis.
These patterns apply to any quantitative strategy, not just pairs.

## The approach: Logs → Patterns → Fixes → Rerun → Commit

### 1. Add structured logging FIRST
Before optimizing anything, add persistent logs to every decision point:
- **Entry decisions**: what passed, what was rejected, and why
- **Hold decisions**: per-bar state (z-scores, P&L, drift)
- **Exit decisions**: which condition triggered, what the alternatives were

Log to a file (`data/journal/walkforward.log`), not just stdout.
Include timestamps and session headers so runs are distinguishable.

### 2. Run simulation, read the logs, find patterns
Don't look at aggregate stats first. Read individual trades:
```
python3 scripts/pair_deep_dive.py "FDX/UPS"
```
Look for:
- Trades where fixed_z gets WORSE over the hold (spread trending, not reverting)
- Entries with nonsensical parameters (negative beta, tiny spread_std)
- Exit reasons: is everything hitting max_hold? That means reversion isn't happening
- Rolling z vs fixed z drift: large drift = the old rolling-exit bug in action

### 3. Fix one thing, commit, rerun, compare
Each fix gets its own commit with before/after numbers:
```
afe8597: 58 trades, 41% win, -$5,164  (baseline)
ab27884: 51 trades, 43% win, -$2,362  (beta + std fixes)
ebb68d1: 29 trades, 69% win, +$3,397  (quality gate)
```
The commit message IS the experiment log. Include the P&L delta.

### 4. Use statistics on the logs, not just intuition
After fixing obvious bugs, mine the logs for subtler patterns:
- **Group by exit reason**: reversion exits avg +$452, max_hold exits avg -$153
- **Group by entry quality**: R² > 0.95 trades outperform R² 0.85-0.90
- **Autocorrelation analysis**: which pairs actually mean-revert vs trend

### 5. Parameter sweeps belong on branches
Don't tune parameters on main. Create an experiment branch:
- Sweep one parameter at a time (z threshold, max_hold, HL range)
- Log results in commit messages
- Only merge to main if the improvement is structural, not just parameter fitting

## Specific bugs we found through logs

### Bug: Negative beta allowed
- **How found**: COST/WMT entry log showed `beta=-0.4641`
- **Fix**: Guard `beta < 0.1` in scan_pair()
- **Impact**: Removed 7 losing trades

### Bug: No ADF filter despite docs claiming one
- **How found**: Comparing module docstring ("ADF < -2.0") vs actual code (no ADF call)
- **Fix**: Wire in existing `adf_test()` function
- **Impact**: Blocked pairs with spurious regression

### Bug: Spread std too small → z-score stuck at extreme
- **How found**: V/MA entry log showed `z=-9.46` with `spread_std=0.004`
- **Fix**: Guard `spread_std < 0.005`
- **Impact**: Prevented entries that could never cross exit threshold

### Bug: Rolling z decay producing false exits (the big one)
- **How found**: Per-bar logs showing rolling_z decaying while fixed_z stayed extreme
- **Fix**: ExitContext with frozen entry-time stats (Rust engine #182)
- **Impact**: Architectural fix — prevented entire class of false reversion signals

## Pattern analysis as a pair selection tool
- Run `python3 scripts/pattern_analysis.py` to generate patterns dashboard
- AC(1) < -0.10 identifies mean-reverting pairs
- Holding period curves show optimal hold per pair (varies: 4d for banks, 8d for tech)
- Day-of-week tables show when each pair is most active
- Spread change distributions reveal fat tails (danger) vs tight (safe)

## Key principle
**The numbers tell you WHAT is wrong. The logs tell you WHY.**
Aggregate P&L says "strategy loses money." Logs say "because we're
entering COST/WMT at beta=-0.46 and every trade hits max_hold because
the spread never reverts." That's actionable.
