# Prospective source identity v1

A standalone, offline evaluator component for **future, explicitly opted-in
cohorts**. It reads selected source bytes, resolves a declaration spelling, and
checks exact citations. It does not use graph output, import evaluated Python
modules, expand Rust macros, invoke providers, or run evaluated source. It uses
Python 3.10+ and an isolated, pinned Rust syntax frontend (the executable
snapshot and generic-Python fixture use Python 3.12). The frontend parses source
with `syn`; it never passes evaluated bytes to a compiler.

Nothing here changes historical v1/v2 evaluators, imports, corpus, protocols,
locks, answers, reports or scores. These tests demonstrate contract behavior on
new synthetic sources, **not measured agent benefit**. Unsupported syntax is an
explicit limitation, not evidence that an answer is a hallucination.

## Run

From the repository root, on Python 3.12, first build the evaluator frontend.
Rust 1.89+ and the exact crates in `syntax/Cargo.lock` must already be cached;
these commands do not install host software or access the network. Check disk
usage before creating the build directory and stop at 80% or above:

```sh
df --output=pcent .orbit/tmp | tail -1
CARGO_TARGET_DIR="$PWD/.orbit/tmp/source-identity-syntax-target" \
  cargo build --offline --locked --manifest-path evals/source-identity-v1/syntax/Cargo.toml
```

Then run the prospective gates (including the isolated frontend's format/lint
checks, which are separate from the product workspace):

```sh
cargo fmt --manifest-path evals/source-identity-v1/syntax/Cargo.toml --check
CARGO_TARGET_DIR="$PWD/.orbit/tmp/source-identity-syntax-target" \
  cargo clippy --offline --locked --manifest-path evals/source-identity-v1/syntax/Cargo.toml -- -D warnings
cargo deny --offline --locked --manifest-path evals/source-identity-v1/syntax/Cargo.toml \
  check --config deny.toml --disable-fetch
python3 -B -m unittest discover -s evals/source-identity-v1 -p 'test_*.py'
python3 -B evals/source-identity-v1/example.py
python3 -B evals/source-identity-v1/source_identity.py --help
```

`example.py` prints a deterministic, test-only result: identity recall **2/3**,
citation recall **1/3**, one missing required identity, one visible unsupported
alias, and **pending** semantic review. One answer uses an equivalent
trait-qualified Rust spelling; another names the correct Python assignment but
quotes the wrong value. `example-result.json` pins the exact output, contract
bytes, implementation bytes, source bytes, manifest and Python AST version.
The frontend runtime and its complete dependency lock are also pinned.
The snapshot is a fixture, not a study result. Semantic review has no inferred
pass, combined score, or agent-effectiveness interpretation.

To exercise the reusable scorer with the same JSON request:

```sh
mkdir -p .orbit/tmp
python3 -B evals/source-identity-v1/example.py --request > .orbit/tmp/source-identity-request.json
python3 -B evals/source-identity-v1/source_identity.py \
  --root evals/source-identity-v1/fixtures \
  --input .orbit/tmp/source-identity-request.json
```

The CLI writes one JSON result to stdout and exits 0, even when submitted answers
receive no credit. Invalid evaluator inputs/truth exit 2 with a JSON error on
stderr and empty stdout. Argument-parser usage errors also exit 2. `--help` and
representative output are captured from the real executables in `cli-help.txt`
and `example-result.json`. To deliberately regenerate those fixtures after a
reviewed implementation/contract change, run:

```sh
UPDATE_GOLDENS=1 python3 -B -m unittest discover -s evals/source-identity-v1 -p 'test_*.py'
```

Normal tests never rewrite them. A changed runtime version changes the pinned
output; evaluate and adopt that runtime prospectively as well. The repository's
complete validation sequence remains in `CONTRIBUTING.md`; these tests are a
separate local gate because this task does not change the historical harness.

## Input and identity

`source_identity.Project(root, rust_root=..., python_files=[...])` constructs a
read-once source snapshot. `Project.check(selector)` validates an item;
`score(project, required, submitted)` scores arrays; `score_request(root,
request)` is the same entry point as the CLI. The request has exactly:

- `schema_version: 1`;
- `manifest: {"rust_root": "rust/lib.rs", "python_files": ["python/settings.py"]}`
  (`rust_root` may be null);
- a nonempty `required` array and a `submitted` array.

Every selector has exactly `language`, `name`, `file`, `line`, `kind`, and
`citation`. `language` is `rust` or `python`; file paths are canonical,
root-relative POSIX paths; lines are positive, one-based integers (not booleans).
Kinds are `fn`, `struct`, `enum`, `trait` for supported Rust declarations and
`function`, `class`, `assignment` for supported Python declarations. Python
assignments are syntactic bindings, not claims of runtime immutability.

A citation is `{"start_line": 2, "end_line": 2, "quote": "RETRY_LIMIT = 7"}`.
It must begin on the declaration anchor line, end inside that declaration, and
match complete source lines including indentation, joined by LF without a final
newline. Substrings, different values, nearby comments, empty quotes, and
out-of-span ranges do not receive citation credit. Source encoding is UTF-8/LF;
CRLF, NUL, symlink paths, escaping paths and files over 1 MiB are rejected.

Canonical identity consists of language, file, line, column, declaration kind,
module, verified owner, verified trait and leaf name. Rust's anchor is its name
token; Python's is the AST declaration/assignment start. Function bodies and
class spans provide citation bounds, not additional name-resolution scopes.
Hashes bind those locations to the exact source snapshot. Rust module identity
comes from the selected crate root and module declarations, **not filenames**.
Python module identity is the explicit root-relative `.py` path, with
`__init__.py` naming its package. It does not inspect `sys.path`.

Resolve spellings across the **entire selected language index before** comparing
file, line and kind. A line number cannot break a duplicate-name tie. An
unqualified free function name considers free declarations, not methods sharing
its leaf. Use `crate::parse` or `nested::parse` to distinguish free functions in
different modules. `Compass::parse` and `Ledger::parse` identify different owners.
For owner-qualified methods, a short owner must itself be unique across indexed
types/import bindings, even if only one owner defines the method.

For the synthetic trait implementation, these identify the same declaration:

- `Compass::from_str` (only when no other trait/inherent declaration competes);
- `<Compass as FromStr>::from_str` (the import verifies `FromStr`);
- `<crate::Compass as std::str::FromStr>::from_str`.

Validated local type/import aliases and full module-qualified names are
supported. Trait-qualified names must match the verified trait binding. Rust
**dot spellings are rejected** with a reason: use `Owner::method`, not
`Owner.method`. There is no heuristic conversion or observed-name allowlist.

## Bounded source grammar

`syntax/` is a separate, unpublished Cargo workspace with no product dependency
edges. It uses `syn` **2.0.119** (full syntax and AST visitor) and `proc-macro2`
**1.0.106** (lexing and source spans), with JSON transport and typed errors.
`syntax/Cargo.lock` pins every transitive version, registry origin and checksum.
These crates came from the existing Cargo registry cache; their package manifests
and license files are the upstream provenance, not recovered Orbit code. The
parser and lexer are MIT/Apache-2.0 dual licensed. No vendored or downloaded
parser binary is committed. Build the frontend from these sources and lockfile
in the ignored path above. The scorer launches that fixed executable with an
empty environment and a ten-second timeout; it never builds on demand.

The frontend parses the **entire file**, including function parameters, return
types, bodies, literals, fields and unrelated declarations. Syntax errors and
unrecognized `Verbatim` nodes refuse the Rust index. Validated source passes to
the declaration resolver; delimiter balance alone cannot grant credit. The
binary embeds its own source, manifest and lockfile, which Python compares with
the current files before accepting a successful parse. Missing, stale, failing
or timed-out frontends refuse Rust credit with a diagnostic. Returned pins hash
these files alongside the Python implementation, contract and source snapshot.
This detects accidental stale builds; it is not executable attestation or a
security boundary against an evaluator who controls the helper binary.

Supported declarations include free/generic functions, structs/enums with
parameters and fields, local traits with supertraits/associated items, and
inherent/trait impl methods of verified local named types, including generic
owners and where clauses. Owner/trait arguments are erased **only for spelling
resolution of a written declaration**. Two specialized impls defining the same
method stay ambiguous; this never infers instantiation, trait applicability or
runtime dispatch. Blanket impls on generic parameters, qualified-self types and
negative impls remain unsupported.

Ordinary line/block docs, explicit `doc`, lint attributes, `repr`, `inline`,
`cold`, `must_use`, `deprecated`, `non_exhaustive`, `no_mangle`, `export_name`,
`link_section` and `track_caller` preserve written binding identity. Standard
`derive` entries (`Clone`, `Copy`, `Debug`, `Default`, `Eq`, `PartialEq`, `Ord`,
`PartialOrd`, `Hash`) are admitted; generated methods are never indexed. Custom
attribute/derive macros and `cfg_attr` are refused because they can emit unknown
bindings. This is an explicit attribute-effects policy, not an ignore-all rule
or an allowlist of declaration names. Attribute argument semantics are not
compiler-checked.

A `cfg` declaration is recorded as uncertain, with all descendants/methods of
conditional modules, owners, traits and impls also uncredited. It still competes
in ambiguity checks; a line number cannot rescue it. Named const/static/type
alias/associated bindings are unsupported candidates and blockers, so they do
not erase independently verified unrelated declarations. Macro definitions (whose matcher grammar is not validated by `syn`),
item macro invocations, foreign scopes,
glob/group/conditional imports and other contexts with an unknown binding set
still refuse the whole Rust index. Diagnostics explain the distinction.

Inline modules and external modules with literal `#[path = "file.rs"]` retain
the explicit closed source universe. Paths work at the crate root and within
its inline modules; external files cannot declare further external modules.
Reused/cyclic files, duplicate modules, ordinary external modules without a
literal path, import collisions/cycles and shadowed `std`/`core` roots are
refused. Simple `use path;` / `use path as Alias;` can reference verified local
types or written `std::`/`core::` paths. Unknown owners/traits and type aliases
used as impl owners are refused. No extern-crate/prelude guessing occurs.

Syntax validity means acceptance by the pinned parser's Rust grammar, **not
compilation, typechecking or behavioral correctness**. Macro token trees remain
unexpanded (their tokens need not themselves be Rust expressions), and methods
generated by macros/derives are not written declarations. Edition/feature
availability, name/type resolution inside bodies, trait completeness, attribute
semantics and runtime behavior require separate review. The synthetic sources
include syntactically valid but semantically incomplete bodies. The helper is
neither a compiler nor a security sandbox.

Python uses `ast.parse`, never `import`, `eval`, `exec`, or `ast.literal_eval` on
source. It supports free/async functions (including generic syntax supported by
the pinned AST runtime), simple classes/methods, and single module-level named
assignments/annotated assignments whose values are literal scalars or recursively
literal tuples. Signed numeric literals are allowed. Names, calls, arithmetic,
mutable containers, annotation-only bindings, complex annotations and assignment
aliases stay unsupported. Explicit imports are recorded as unsupported bindings
without importing them. They still block ambiguous names. Duplicate/rebound
bindings cannot be rescued by quoting one occurrence. Decorated functions receive
no credit; decorated/inherited/rebound classes invalidate the Python index so
hidden method ownership cannot be inferred. Destructuring, chained/attribute
assignment, wildcard imports, conditionals, mutation and other module/class
statements invalidate the Python index. Function bodies are opaque AST subtrees;
local/nested function bindings are not scored.

These are **syntactic declaration identities**, not evaluation of Python module
initialization or arbitrary import/decorator/call effects. A dynamic RHS does not
become a constant merely because an answer cites it. Establish runtime claims
through the separate semantic review. When one source file makes a language
index incomplete, all identities in that language are refused; another complete
language index can still be evaluated, with the refusal visible in output.

## Scoring and prospective adoption

The evaluator author must freeze the source root, manifest, implementation,
contract, runtime and required identities **before** collecting answers. The
manifest is trusted evaluator input, never selected independently by each answer:
a smaller file universe can hide ambiguity. Keep source files stable while the
snapshot is constructed. Files are cached once read; later disk changes do not
alter that snapshot's citations or hashes. No repository-root discovery or graph
lookup supplies missing context.

Required identities and their citations must all validate, with no duplicate
canonical identities. Invalid/unsupported truth fails the run; it cannot shrink
the denominator. Submitted duplicates cannot increase recall. Identity recall
counts distinct required identities; citation recall counts those same identities
with exact evidence. Every submitted item survives in `items`, including
`verified_extra` declarations and uncredited/unsupported items with reasons.
`missing` retains every unsatisfied required identity. There is no automatic
penalty that labels an unsupported item hallucinated.

`semantic_review` always remains pending. Consumers must retain this independent
review and must not treat identity/citation equivalence as semantic correctness.
Future cohorts must deliberately record and verify the returned contract,
implementation, source and runtime pins in their own prospective lock/protocol.
Never apply the new interpretation to frozen historical scores or import it into
historical evaluators. Contract/grammar changes require a new prospective pin
(and a new contract version for changed identity semantics).

The fixtures use independently invented `Compass`, `Ledger`, `Dial`, `Socket`
and Python bindings. Tests include trait/inherent ambiguity, owner ambiguity,
wrong module/file/kind/line/quote, literal path mapping, imports, conditional
alternatives, dynamic assignments, comments/string lookalikes, malformed source,
symlink refusal, source snapshot hashing, unchanged denominators, and executable
success/error/help behavior. They are deterministic and run without providers,
network access, or host installation. Build the pinned frontend offline first.
