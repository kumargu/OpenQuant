
## Bug: Duplicate symbol exposure across pairs

NTRS was short in TWO pairs (KEY/NTRS and CFG/NTRS) simultaneously,
creating double concentration risk. If NTRS moves against us, both
pairs lose.

**Fix needed**: Before entering a pair, check if either leg already
has an open position in any other pair. Block entry if so.

This is a portfolio-level check in PairsEngine::on_bar(), not per-pair.
Similar to max_concurrent_pairs but checking symbol overlap instead of
pair count.
