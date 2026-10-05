# Fixture truth review

Truth was initially authored from literal synthetic sources before querying the
candidate. Post-query source-contract errata are recorded in capture-review.md;
the amended protocol is diagnostic, not a clean preregistered study. No candidate output is
an expected-output oracle. Each case's `source_review` explains the eligible
source fact; `checks` declares exact complete sets where set precision/recall is
reported. The fixture manifest is independent of candidate/source identities.

The registry at `crates/orbit-graph-extract/src/languages/mod.rs` registers Rust,
Python, C, C#, Go, Java, JavaScript, TypeScript, Kotlin, Ruby, markdown and config.
JSX/TSX extension variants get their own cases; these bytes exercise ordinary
syntax and extension routing, not JSX markup or two additional extractors. Small language fixtures each
write one import, two callable declarations and one unqualified local call.
Java/C# additionally declare the owning App class. Java's extractor distinguishes
method arity (`App::entry#0`/`App::helper#0`); Ruby top-level defs have method kind.
The syntactic external imports are intentional: fixtures do not claim to compile
or depend on a network/package registry. Resolution expectations follow the
documented same-file/explicit-import confidence ladder, never a compiler model.

The graph fixture's adjacency is root -> {left,right} -> leaf and
cycle_a <-> cycle_b. Direction and depth determine complete reached sets.
The 205-leaf hub exceeds the fixed 200-node impact cap. The Rust command fixture
has one Subcommand Run whose dispatch arm calls run -> middle -> leaf. A second
fixture duplicates commands in two files to challenge undisclosed root choice.
The UTF-8 show span was calculated from the literal UTF-8 bytes, including café
and the planet; it is a half-open byte span, not a character count.

The Python ambiguity fixture has A.work and B.work. A runtime worker receiver
does not establish either owner; a callback parameter has no concrete target;
mystery is external and __import__ is runtime binding. Fuzzy unresolved output
or explicit omission is valid. Exact concrete ownership is overconfidence.
Macro expansion is outside written-call truth. Markdown headings, nested TOML
keys and unsupported .proto sources have separate units and denominators.

The original no-call lifecycle bytes remain preserved. The corrected lifecycle
fixture additionally writes keep -> remove in base and keep -> added in head.
Lifecycle truth starts with keep/remove and old.rs:moved. The next frozen tree
retains keep, adds added, deletes remove and renames old.rs to new.rs. First,
full and incremental publication are measured independently and compared with
a fresh full overview, four separate literal search queries, callees and refs.
Both incremental and full outputs must independently satisfy complete source
identity/edge sets; equality of two wrong results cannot pass. History freshness and code-index freshness are separate
claims; missing history must be an error. Outdated state is created by changing
the frozen checkout after indexing its base.

Change fixtures separately add, remove, move, rename with body edit, replace
multiple identical bodies, modify an unsupported file, and modify only a test.
Every comparison and occurrence is pinned to the exact frozen base/head commits.
Base-only/head-only sides must survive. Exact body-preserving movement may be
paired; equal duplicate bodies and rename-plus-edit cannot establish a confident
body identity; every old/new occurrence remains required, including uncertain
candidates. The corrected budget fixture changes leaf's body while hub calls it;
node_cap=1 cuts traversal. Budget and max-symbol cases require explicit incompleteness/cuts.

Synthetic recommendation chronology is fixed: training text at 50, delivery at
200; a file rename and deletion at 300; target task text at 400, query cutoff
500, genuinely later heldout modification plus addition at 600. The target tree
has only renamed.rs. Added new.rs is absent at the query time, so it is omitted
from retrospective target-file and target-symbol denominators. Heldout truth
never enters training. Public-envelope provenance is synthetic; plugin import
may honestly label the caller's verified-delivery assertion as caller-attested.
The standalone chronological evaluator validates Git boundaries and clocks;
it runs all four variants at file and symbol levels. The sole live file/symbol
matches both historical task text and current lexical parse text, so each variant
has one returned true positive: recall 1, precision 1/10 and stale fraction 0.
Reported counts and formulas are checked independently of score magnitudes.
The private Git object database contains later commits: this tests cutoff
behavior, not physical inaccessibility of future objects. Empty admitted cohorts must
produce null metrics. No real tasks/runs, commit-text query, provider or model
contributes evidence.

Independent source-only reviews from codex:/root and its attributed reviewers
are attached to ORB-14004 under operator/source-truth-20261005T0057/:
changes-history-review.json, graph-truth-review.json and python-language-review.json.
The root's 01:19 UTC import addendum supersedes the initial null-symbol review for
Rust, C# and Kotlin. Their findings and the 01:38 scorer recheck are reconciled in
capture-review.md. These reviews read no candidate outputs. Review of corrected
source bindings completed independently in final-source-reconciliation-20261005T0155.json
for corpus 3463055d4023af35ea2aef750b67ec074dcbf8f99ddb63ee4badabaa19deadb6
and manifest 92c24f4edcfaadab7adc22b4359a190bbd3330dc2633de62c2b0a687f0f28600.
Scorer controls were independently verified in scorer-controls-20261005T0153.json.
Final captured-report review remains for the operator; executor testing does not
certify operator approval.

Coverage units are not additive: the unsupported and omitted file in the coverage
fixture are the same schema.proto (three physical files, two indexed). The 95
scenarios reuse tiny repositories and do not establish broad language correctness.
