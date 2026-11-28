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
   references resolved as the caller, then validated). No other path calls the
   validator. A new write path calls it, or it is a bug. (Conditional references were
   resolved on three paths and stored as written on four others.)
4. **What the HL7 validator accepts, sazare does not refuse.** Where they differ, the
   validator's verdict decides; a deliberate difference is listed with its reason
   (`ci/validator_compare.py`, `KNOWN_SAZARE_ONLY`). Ask the validator itself, not a
   review or memory: both were wrong about HTML entities and attributes.
5. **Nothing is ignored silently.** What sazare doesn't support answers a 400 that says
   so; it is never dropped and answered as if done (a search with an unknown
   parameter, a stored conditional reference, an invalid date that left the index).

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
store schema change is a minor version. A behaviour change is listed under
**Behavior changes** in the CHANGELOG. Docs and release notes are in English.
