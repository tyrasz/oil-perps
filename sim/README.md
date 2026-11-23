# Trading simulation

An agent-based simulation that drives the real programs in-process:
perps-core, mm-registry and orderbook. Set `SBF_OUT_DIR` to run their SBF
binaries. The agents:

- 2 market makers quoting on mm-registry;
- retail traders: momentum, mean-reversion, high-leverage, limit-order and
  house (`open_position`);
- a keeper that matches, runs funding, liquidates and closes expired orders.

They trade one OIL market on the CME session schedule, Wednesday through the
weekend reopen, against a random-walk oracle with jumps and weekend gaps.

```sh
cargo run -p sim --release -- --scenario normal --seed 1          # native
scripts/build-sbf.sh                                              # then, on SBF:
SBF_OUT_DIR=$PWD/target/deploy cargo run -p sim --release -- --scenario stress --seed 1 --report out.json
```

Options: `--scenario normal|stress|adversarial`, `--seed N`, `--days D` (default 5),
`--tick-secs S` (default 300), `--report path.json`, `--strict-solvency`.

- **stress** adds 10–20% gaps at every reopen, 10x positions held into the
  gaps, and orders larger than the venues' per-fill cap, so bad debt and
  fill splitting happen.
- **adversarial** adds colluding pairs crossing at the band edge at maximum
  leverage, wash trades, a prober that tries every approval and venue
  rejection, a guardian pause, and `settle_market` at the end.

Every transaction's outcome is predicted by an independent model of the
engine (`model.rs`, `predict.rs`, `schedule.rs`). It checks, each tick and
at the end:

- open interest;
- fill and settlement accounting, including bad debt;
- liquidation predicates (the engine's vs the model's);
- exact allowance tracking;
- that matched pairs settle as a zero-sum pair;
- the session schedule;
- final token conservation.

Any violation, or any error the model did not predict, makes it exit 1.

**Solvency** (invariant 2) is reported as a finding, not a failure, unless
`--strict-solvency` is passed. It is a known engine design issue: house
positions and orphaned venue legs have the shared vault as their counterparty,
and nothing ties that exposure to the insurance fund. The report prints it
under `!!! FINDING`.
