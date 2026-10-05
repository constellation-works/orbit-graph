"""Prospective admission bindings. External attestations are not authentication."""
from datetime import datetime, timezone
from pathlib import Path
import re
from rust_source import PARSER

VERSION = "graph-effectiveness-runtime-v2"
HARNESS = ("eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py")


def timestamp(value):
    if not isinstance(value, str) or re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", value) is None:
        raise ValueError("attestation timestamp must be UTC RFC3339 seconds")
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def request_plan(ev, value, plan, corpus, protocol, lock):
    ev.source.shape(value, ["schema_version", "profile", "model", "provider_binary_sha256", "harness", "requests"], "profile plan")
    ev.require(value["schema_version"] == 3 and value["profile"] == ev.PROSPECTIVE_PROFILE,
               "prospective schema-3 installed-plugin-skill-v3 required; historical v2 cannot qualify")
    ev.require(len(value["requests"]) == len(plan["slots"]), "missing scheduled requests")
    expected = ev.requests(corpus, protocol, lock, plan, value["requests"][0]["plugin"])
    ev.require(value["requests"] == expected, "request/source/treatment pin drift")
    ev.require(value["harness"] == {p:ev.sha((ev.REPO / "scripts/agent-eval" / p).read_bytes()) for p in HARNESS}, "harness pins")
    ev.source.shape(value["model"], ["provider", "name", "version", "settings"], "model pins")
    ev.require(value["model"]["provider"] == "codex-cli" and all(isinstance(value["model"][k], str) and value["model"][k] for k in ("name", "version"))
               and isinstance(value["model"]["settings"], dict), "exact model/settings pins required")
    ev.require(isinstance(value["provider_binary_sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", value["provider_binary_sha256"]), "provider binary pin")
    ev.require(all(k in ev.runner.SETTING_KEYS or k == ev.runner.CODE_MODE_FEATURE and v == "enabled"
                   for k,v in value["model"]["settings"].items()), "unrecognized model setting pin")
    return expected


def evidence(ev, records):
    ev.require(isinstance(records, list) and records, "external evidence files required")
    for record in records:
        ev.source.shape(record, ["path", "sha256"], "external evidence")
        path = Path(record["path"])
        ev.require(path.is_absolute() and not path.is_symlink() and path.is_file(), "external evidence must name an original regular absolute file")
        ev.require(path.stat().st_size <= 64 * 1024 * 1024, "external evidence exceeds 64 MiB: " + str(path))
        ev.require(ev.sha(path.read_bytes()) == record["sha256"], "external evidence changed: " + str(path))


def freeze(ev, plan, rp, corpus, protocol, lock, export, views, truth, qualification, custody):
    """Validate exact bytes and attributed assertions; root independently attests identity/custody."""
    ev.checked_plan(plan, corpus, protocol)
    request_plan(ev, rp, plan, corpus, protocol, lock)
    ev.require(truth is not None and views is not None and export is not None, "live admission requires full export and independent truth reviews")
    admission = ev.scoring.truth_admission(corpus, export, truth, views)
    ev.require(not admission["pending_cases"] and admission["independent_diff_review"] == "verified", "independent truth identity/semantic/diff review pending")
    ev.require(qualification is not None and custody is not None, "live admission requires external runtime qualification and operator custody")
    ev.source.shape(qualification, ["schema_version", "operator", "recorded_at", "method", "verdict", "request_plan_sha256",
                                  "lock_sha256", "tool_versions", "resource_limits", "bwrap", "identity_frontend_sha256",
                                  "evidence", "qualification_sha256"], "runtime qualification")
    ev.source.check_seal(qualification, "qualification_sha256")
    ev.require(qualification["schema_version"] == 1 and qualification["operator"] and
               qualification["method"] == "strict-schema3-namespace-client" and qualification["verdict"] == "qualified",
               "root must externally attest strict namespace/client qualification")
    ev.require(qualification["request_plan_sha256"] == ev.digest(rp) and qualification["lock_sha256"] == ev.digest(lock), "qualification runtime pins differ")
    ev.source.shape(qualification["tool_versions"], ["read", "rg", "git"], "qualified baseline binaries")
    ev.require(all(isinstance(v, str) and re.search(r"sha256:[0-9a-f]{64}$", v) for v in qualification["tool_versions"].values()), "exact baseline binary pins required")
    ev.source.shape(qualification["resource_limits"], ["memory.max", "pids.max"], "qualified resource ceilings")
    ev.require(all(type(v) is int and v > 0 for v in qualification["resource_limits"].values()), "finite qualified resource ceilings required")
    ev.source.shape(qualification["bwrap"], ["version", "sha256"], "namespace binary")
    ev.require(qualification["bwrap"]["version"] and re.fullmatch(r"[0-9a-f]{64}", qualification["bwrap"]["sha256"]), "namespace binary pin required")
    frontend = PARSER
    ev.require(frontend.is_file() and ev.sha(frontend.read_bytes()) == qualification["identity_frontend_sha256"], "identity frontend binary differs; build and qualify exact helper")
    evidence(ev, qualification["evidence"])
    ev.source.shape(custody, ["schema_version", "study_kind", "operator", "recorded_at", "plan_sha256", "request_plan_sha256",
                            "truth_reviews_sha256", "qualification_sha256", "attestations", "evidence", "custody_sha256"], "operator custody")
    ev.source.check_seal(custody, "custody_sha256")
    ev.require(custody["schema_version"] == 1 and custody["study_kind"] == "agent" and custody["operator"], "external agent-study operator custody required")
    ev.require(custody["plan_sha256"] == plan["plan_sha256"] and custody["request_plan_sha256"] == ev.digest(rp) and
               custody["truth_reviews_sha256"] == ev.digest(truth) and custody["qualification_sha256"] == qualification["qualification_sha256"], "custody pins differ")
    fields = ["schedule_frozen_before_output", "truth_review_identity_independently_attested", "serial_dispatch",
              "original_capture_custody", "source_transfer_authorized", "non_synthetic_provider"]
    ev.source.shape(custody["attestations"], fields, "custody attestations")
    ev.require(all(custody["attestations"][k] is True for k in fields), "operator custody attestation incomplete")
    evidence(ev, custody["evidence"])
    frozen_at = timestamp(custody["recorded_at"])
    ev.require(timestamp(qualification["recorded_at"]) <= frozen_at <= datetime.now(timezone.utc), "qualification must precede the operational freeze")
    ev.require(all(timestamp(r["reviewed_at"]) <= frozen_at for r in [*truth["reviews"], *truth["adjudications"]]), "truth review must precede the operational freeze")
    # An attestation cannot promote the shipped fake provider by renaming its model.
    fake = ev.REPO / "scripts/agent-eval/tests/fake_codex.py"
    ev.require(not fake.is_file() or rp["provider_binary_sha256"] != ev.sha(fake.read_bytes()), "synthetic fixture provider cannot become live evidence")
    ev.require(not any(word in (rp["model"]["name"] + " " + rp["model"]["version"]).lower() for word in ("fake", "test-only", "synthetic")), "synthetic model cannot become live evidence")
    return ev.seal(dict(schema_version=1, runtime_version=VERSION, study_kind="agent", plan_sha256=plan["plan_sha256"],
                        request_plan_sha256=ev.digest(rp), lock_sha256=ev.digest(lock), source_sha256=ev.digest(lock["snapshots"]),
                        truth_reviews=truth, qualification=qualification, custody=custody,
                        boundary="Code validates pins, source reviews, evidence bytes and timestamp order; identity, pre-output freeze and temporal custody are externally attested, not authenticated by a hash."), "freeze_sha256")


def checked_freeze(ev, value, plan, rp, corpus, protocol, lock, export, views):
    ev.require(value is not None, "live admission requires --freeze with independent truth, qualification and custody")
    ev.source.check_seal(value, "freeze_sha256")
    ev.require(value == freeze(ev, plan, rp, corpus, protocol, lock, export, views, value["truth_reviews"],
                              value["qualification"], value["custody"]), "operational freeze differs from exact runtime/source pins")
