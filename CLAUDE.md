# Working on sazare

How changes are made here, for people and for coding agents. The why behind each
rule is in the change that taught it (CHANGELOG, the PR named).

## Design rules

1. **Rules come from the specification, not by hand.** What a resource may hold is
   generated from the R4 definitions (`sazare-core/structure/generate.py` →
   `r4.tsv`, `r4-primitives.tsv`; search parameters likewise); narrative rules follow
   the HL7 validator. A hand-written list of rules is a sign something should be
   generated. (A hand-written list of required elements covered 17 types, top level
   only; one `min` column covers all of them, nested too.)
2. **Fix the cause of a hole, not the hole.** When a defect is found, ask why it could
   exist and remove that: a fix that adds a rule for one case is a patch; one that
   folds cases into a single mechanism is the right direction. (A string search for
   `<script` was bypassed five ways; parsing the XML and checking an allow-list closed
   them all.)
3. **Every write of a resource body goes through `write::prepare`** (conditional
   references resolved as the caller, then validated). No other write path calls the
   validator (`$validate` and `sazare-server check` call it directly: they only inspect,
   storing and rewriting nothing). A new write path calls `prepare`, or it is a bug. (Conditional references were
   resolved on three paths and stored as written on four others.)
4. **What the HL7 validator accepts, sazare does not refuse.** Where they differ, the
   validator's verdict decides; a deliberate difference is listed with its reason
   (`ci/validator_compare.py`, `KNOWN_SAZARE_ONLY`). Ask the validator itself, not a
   review, memory or a reading of its code: all three were wrong about HTML entities,
   attributes and `<thead><td>`. And ask everything, not what comes to mind: try every
   combination, each in a context where only it can decide (the element pairs, each
   counted only when its control passes); asking element by element still missed rules.
5. **Nothing is ignored silently.** What sazare doesn't support answers a 400 that says
   so; it is never dropped and answered as if done (a search with an unknown
   parameter, a stored conditional reference, an invalid date that left the index).

6. **Scope stays narrow.** R4 (R6 next), JSON, the FHIRPath subset search needs. R5, XML or
   full FHIRPath are not added for parity with other servers.
7. **A setting is a promise.** Each configuration key is covered by `STABILITY.md`; add
   one only when it can be kept for good.
8. **The README claims only what CI shows.** A claim without a check behind it (the write
   checks were once described wider than they were) is a defect like any other.

## How changes are checked

- `cargo test --all` and `cargo clippy --all --all-targets -- -D warnings`.
- The R4 examples: `R4_EXAMPLES=… cargo test -p sazare-core -- --ignored r4_examples`
  (see `ci/r4_examples.py`); official examples with errors of their own are listed in
  `KNOWN_INVALID` with reasons, and must keep failing.
- The validator comparison (`ci/validator_compare.py`, the CI `validator` job): the R4
  examples and copies broken many ways, through all ten write paths, against the HL7
  validator. A new rule gets a breakage there; a ratchet (`ci/validator_baseline.json`)
  catches checking less.
- After a rebase, look for conflict markers before committing.

## Releases and compatibility

`STABILITY.md` is the promise; `UPGRADING.md` says what each version changes on disk. A
store schema change is a minor version. Before a release, the comparison with the newest
HL7 validator (`validator-latest.yml`, also monthly) passes, or what differs is decided:
the agreement is with the version CI pins, and the validator's rules change. A behaviour change is listed under
**Behavior changes** in the CHANGELOG. Docs and release notes are in English.
