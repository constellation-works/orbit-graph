# Prospective source identity v1

A standalone, offline evaluator component for **future, explicitly opted-in
cohorts**. It reads selected source bytes, resolves a declaration spelling, and
checks exact citations. It does not use graph output, import evaluated Python
modules, expand Rust macros, invoke providers, or run evaluated source. Its only
dependency is the Python standard library (3.10+; the checked-in executable
goldens and generic-Python fixture pin Python 3.12).

Nothing here changes historical v1/v2 evaluators, imports, corpus, protocols,
locks, answers, reports or scores. These tests demonstrate contract behavior on
new synthetic sources, **not measured agent benefit**. Unsupported syntax is an
explicit limitation, not evidence that an answer is a hallucination.

## Run

From the repository root, on Python 3.12:

```sh
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

`rust_source.py` lexes nested comments, quoted/raw strings, chars and lifetimes,
and balances delimiters before walking items. It recognizes free/generic
functions, simple structs/enums, simple local traits, inherent and trait impls
for verified local nongeneric types, inline modules, and external modules with
literal `#[path = "file.rs"]`. Explicit paths work at the crate root and within
its inline modules; external files cannot themselves declare external modules.
Reused/cyclic files and duplicate modules are refused. Associated type bindings
inside trait impls are skipped, not scored.

Simple `use path;` / `use path as Alias;` can reference indexed local types or
`std::`/`core::` paths. The latter attest the **written trait path**, not the
contents of a standard-library implementation; no dependency source is loaded.
Import collisions/cycles, shadowed `std`/`core` roots, and unknown local owners/traits are refused. There is
no extern-crate/prelude guessing. Traits with associated type declarations,
generic impl owners, supertraits, type aliases, glob/group imports, cfg,
procedural attributes, item macros, `include!`, ordinary external modules without
`#[path]`, associated constants, and Rust const/static items are outside v1.
Unsupported item/context syntax invalidates the whole Rust index: unknown
bindings cannot silently make another spelling unique.

This is a declaration parser, **not a Rust compiler/type checker**. Balanced
function bodies, parameter/type expressions, and struct/enum fields are opaque;
it does not prove that expressions compile, trait impls satisfy their trait, or
runtime behavior is correct. It rejects lexical and item-structural malformations
but does not claim to reject every invalid Rust program. Prospective corpus
preflight must independently establish source validity. The synthetic sources
are declaration fixtures and intentionally include incomplete semantic bodies.
Do not use this component as a compiler or security sandbox.

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
network access, or installing parsers.
