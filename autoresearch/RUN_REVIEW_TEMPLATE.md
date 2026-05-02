# Run Review Template

Use this after every paper/live/replay run. Keep it short, concrete, and append-only.

## review_<name> (<timestamp>)

- **run_type**: `paper` | `live` | `replay`
- **scope**: what this run was trying to do
- **window**: trading date or replay range
- **config**: universe / policy / cap / execution mode
- **status**: succeeded | partial | failed

### Good
- What worked mechanically
- What worked in the signals
- What worked in risk / sizing / execution

### Bad
- What broke mechanically
- What looked weak in the signals
- What created avoidable risk or confusion

### Signal Quality
- High-conviction entries:
- Low-conviction entries:
- Mean reversion already happening:
- Wrong-way / widening trades:

### Metrics
- Filled orders:
- Open positions:
- Gross exposure:
- Net exposure:
- Realized P/L:
- Unrealized P/L:

### Learnings
- Keep:
- Change:
- Stop doing:

### Next Action
- One concrete change or check before the next run
