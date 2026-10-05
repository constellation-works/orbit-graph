"""Schema-3 accounting reconstructed from sealed capture bytes, never historical sums.

Codex JSONL counter scope/cache subset semantics are deliberately unqualified.
An observed sum is a diagnostic, not an observed session total or a price.
"""
import hashlib
import json
from collections import Counter

import eval_broker as broker

CONTRACT = broker.TELEMETRY_CONTRACT
FIELDS = ("input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens", "total_tokens")
COUNTER_SEMANTICS = "codex-jsonl-scope-unqualified-v1"


def check(value, message):
    if not value:
        raise ValueError("telemetry: " + message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def rows(raw):
    """Locations cover exact JSONL line bytes, including the newline when present."""
    offset = 0
    for number, line in enumerate(raw.splitlines(keepends=True), 1):
        location = {"line": number, "byte_offset": offset, "bytes": len(line), "sha256": sha(line)}
        offset += len(line)
        try:
            event = json.loads(line, object_pairs_hook=broker._unique,
                               parse_constant=broker._reject_constant)
        except (ValueError, RecursionError, UnicodeError):
            event = None
        yield event if isinstance(event, dict) else None, location


def usage(raw, truncated, provider_complete):
    turns, issues, thread = [], [], None
    active = None
    identities = {}
    session_events = 0
    def identity(event, field, location):
        if field not in event:
            return None
        value = event[field]
        try:
            valid = isinstance(value, str) and 0 < len(value.encode()) <= 128
        except UnicodeError:
            valid = False
        if not valid:
            issues.append({"code": "invalid_identity", "field": field, "event": location})
            return None
        return value
    for event, location in rows(raw):
        if event is None:
            if raw[location["byte_offset"]:location["byte_offset"] + location["bytes"]].strip():
                issues.append({"code": "malformed_event", "event": location})
            continue
        kind = event.get("type")
        if kind == "error" or (kind == "item.completed" and isinstance(event.get("item"), dict)
                               and event["item"].get("type") == "error"):
            issues.append({"code": "provider_error", "event": location})
        if kind == "thread.started":
            session_events += 1
            if session_events > 1:
                issues.append({"code": "duplicate_session", "event": location})
            supplied = identity(event, "thread_id", location)
            if thread is not None and supplied is not None and supplied != thread:
                issues.append({"code": "cross_session_identity", "event": location})
            if thread is None:
                thread = supplied
        if kind == "turn.started":
            if active is not None:
                issues.append({"code": "overlapping_turn", "event": location})
            supplied_session = identity(event, "thread_id", location)
            supplied_turn = identity(event, "turn_id", location)
            if thread is not None and supplied_session is not None and supplied_session != thread:
                issues.append({"code": "cross_session_identity", "event": location})
            if thread is None:
                thread = supplied_session
            if supplied_turn is not None:
                key = broker.canonical([supplied_session or thread, supplied_turn])
                if key in identities:
                    issues.append({"code": "duplicate_turn_id", "event": location})
                identities[key] = len(turns) + 1
            active = {"ordinal": len(turns) + 1, "session_id": supplied_session or thread,
                      "turn_id": supplied_turn, "start_event": location, "observations": [],
                      "outcome": "pending"}
            turns.append(active)
        elif kind in {"turn.completed", "turn.failed", "turn.cancelled"}:
            supplied_ids = {key: identity(event, key, location) for key in ("thread_id", "turn_id")}
            supplied_session = supplied_ids["thread_id"]
            if thread is not None and supplied_session is not None and supplied_session != thread:
                issues.append({"code": "cross_session_identity", "event": location})
            if thread is None:
                thread = supplied_session
            if active is None:
                # Keep duplicate terminal observations on their previous turn;
                # an orphan terminal is an expected but unproven turn.
                if turns:
                    target = turns[-1]
                    issues.append({"code": "duplicate_terminal", "event": location})
                else:
                    target = {"ordinal": 1, "session_id": supplied_session or thread,
                              "turn_id": supplied_ids["turn_id"], "start_event": None,
                              "observations": [], "outcome": "pending"}
                    turns.append(target)
                    issues.append({"code": "missing_turn_start", "event": location})
            else:
                target = active
            for field in ("thread_id", "turn_id"):
                supplied = supplied_ids[field]
                key = "session_id" if field == "thread_id" else field
                if supplied is not None:
                    if target[key] is not None and target[key] != supplied:
                        issues.append({"code": "turn_identity_mismatch", "event": location})
                    else:
                        target[key] = supplied
            if target["turn_id"] is not None:
                identity_key = broker.canonical([target["session_id"], target["turn_id"]])
                if identity_key in identities and identities[identity_key] != target["ordinal"]:
                    issues.append({"code": "duplicate_turn_id", "event": location})
                identities[identity_key] = target["ordinal"]
            reported = event.get("usage")
            values = {name: None for name in FIELDS}
            observed, invalid = [], []
            if "usage" in event and not isinstance(reported, dict):
                invalid.append("usage_not_object")
            if isinstance(reported, dict):
                for name in FIELDS:
                    # The fake and some providers spell this reasoning_output_tokens.
                    keys = [name] + (["reasoning_output_tokens"] if name == "reasoning_tokens" else [])
                    keys = [key for key in keys if key in reported]
                    if keys:
                        observed.append(name)
                        value = reported[keys[0]]
                        if len(keys) != 1 or type(value) is not int or value < 0:
                            invalid.append(name)
                        else:
                            values[name] = value
                if (values["cached_input_tokens"] is not None and values["input_tokens"] is not None
                        and values["cached_input_tokens"] > values["input_tokens"]):
                    invalid.append("cached_exceeds_input")
            observation = {"event": location, "type": kind, "reported_fields": observed,
                           "raw_usage": reported, "values": values, "invalid": invalid}
            target["observations"].append(observation)
            target["outcome"] = kind.removeprefix("turn.")
            active = None
    stream_complete = (not truncated and (not raw or raw.endswith(b"\n")) and not issues)
    session_complete = (provider_complete and stream_complete and bool(turns)
                        and all(t["start_event"] is not None and t["outcome"] == "completed"
                                and len(t["observations"]) == 1 for t in turns))
    invalid = any(o["invalid"] for t in turns for o in t["observations"])
    fields = {}
    for name in FIELDS:
        observations = [o for t in turns for o in t["observations"] if name in o["reported_fields"]]
        valid = [o["values"][name] for o in observations if o["values"][name] is not None]
        reported_turns = sum(any(name in o["reported_fields"] for o in t["observations"]) for t in turns)
        state = ("inconsistent" if invalid or issues else
                 "complete" if session_complete and reported_turns == len(turns) else "partial")
        # One clean turn is an observed counter; scope ambiguity only prevents
        # multi-turn aggregation. No assumption about cached subset is made.
        total = valid[0] if state == "complete" and len(turns) == len(valid) == 1 else None
        fields[name] = {"expected_turns": len(turns), "reported_turns": reported_turns,
                        "valid_observations": len(valid), "observed_sum": sum(valid) if valid else None,
                        "coverage": state, "total": total,
                        "total_state": "observed_single_turn" if total is not None else
                        "unknown_semantics" if state == "complete" else state}
    return {"counter_semantics": COUNTER_SEMANTICS, "cache_semantics": "unqualified",
            "session_complete": session_complete, "stream_truncated": truncated,
            "turns": turns, "issues": issues, "fields": fields,
            "derived": {"uncached_input_tokens": None, "state": "unknown_cache_semantics"},
            "cost_usd": None, "per_tool_tokens": None}


def attempts(raw, calls, truncated=False):
    """Reconstruct every parsed tools/call, checking exact request/call/reply links.

    Incomplete evidence stays pending/interrupted. Completed-call records and
    their lossless proofs remain independent, mandatory evidence.
    """
    parsed = list(rows(raw))
    coverage_issues = []
    if raw and not raw.endswith(b"\n"):
        coverage_issues.append({"code": "truncated_tail", "event": parsed[-1][1]})
        if parsed[-1][0] is None:
            parsed.pop()  # retain the complete, immutable prefix; never invent a final attempt
    if truncated:
        coverage_issues.append({"code": "capture_truncated", "event": None})
    coverage = {"state": "partial" if coverage_issues else "complete", "issues": coverage_issues}
    check(all(e is not None for e, _ in parsed), "malformed broker evidence")
    starts = [(e, loc) for e, loc in parsed if e["type"] == "start"]
    if not starts:
        check(not parsed, "broker session start missing")
        return [], {"attempts": 0, "responses": 0, "completed": 0, "successful": 0, "graph_attempts": 0,
                    "graph_completed": 0, "graph_successful": 0, "input_bytes": 0,
                    "output_bytes": 0, "all_attempt_denominator": None if coverage_issues else 0,
                    "coverage": dict(coverage, reported_attempts=0, expected_attempts=None if coverage_issues else 0),
                    "by_tool": {}}
    check(len(starts) == 1 and starts[0][0].get("telemetry_contract") == CONTRACT,
          "broker telemetry version differs")
    for kind in ("request", "call_start", "call", "reply"):
        entries = [e for e, _ in parsed if e["type"] == kind]
        check(all(type(e.get("seq")) is int and e["seq"] > 0 for e in entries)
              and len({e["seq"] for e in entries}) == len(entries), "duplicate/invalid " + kind + " sequence")
    requests = {e["seq"]: e for e, _ in parsed if e["type"] == "request"}
    call_rows = {e["seq"]: e for e, _ in parsed if e["type"] == "call"}
    call_starts = {e["seq"]: e for e, _ in parsed if e["type"] == "call_start"}
    reply_rows = {e["seq"]: e for e, _ in parsed if e["type"] == "reply"}
    interrupted = any(e["type"] == "broker_exit" for e, _ in parsed)
    interruption = next((loc for e, loc in parsed if e["type"] == "broker_exit"), None)
    ledger, by_id = [], {}
    for e, location in parsed:
        kind = e["type"]
        if kind == "attempt_start":
            identity = e.get("attempt_id")
            check(identity == "request-" + str(len(ledger) + 1) and identity not in by_id,
                  "duplicate/nonsequential attempt id")
            seq = e["request_seq"]
            check(seq is None or (type(seq) is int and seq > 0), "invalid attempt request sequence")
            request = requests.get(seq)
            check(seq is None or (request is not None and request.get("method") == "tools/call"
                                 and request.get("request_id") == e["request_id"]), "attempt request differs")
            params = e["params"]
            arguments = params.get("arguments", {}) if isinstance(params, dict) else None
            tool = params.get("name") if isinstance(params, dict) else None
            text = broker.canonical(arguments)
            check(type(e["started_ns"]) is int and e["started_ns"] >= 0, "invalid attempt start")
            row = {"attempt_id": identity, "request_seq": seq, "request_id": e["request_id"],
                   "tool": tool, "arguments": arguments, "input_bytes": len(text.encode()),
                   "input_sha256": sha(text.encode()), "start_event": location,
                   "started_ns": e["started_ns"], "ended_ns": None, "elapsed_ns": None,
                   "end_event": None, "call_seq": None, "completed": False, "successful": False,
                   "call_completed": False, "call_event": None,
                   "measured_output_bytes": None, "measured_output_sha256": None,
                   "outcome": "interrupted" if interrupted else "pending", "error": None,
                   "observed_outcome": None,
                   "output": None, "output_bytes": None, "output_sha256": None,
                   "reply_published": False}
            row["interruption_event"] = interruption if interrupted else None
            by_id[identity] = row
            ledger.append(row)
        elif kind == "call_start":
            row = by_id.get(e.get("attempt_id"))
            check(row is not None and row["call_seq"] is None and not row["completed"]
                  and row["tool"] == e["tool"], "orphan/duplicate attempt call start")
            row["call_seq"] = e["seq"]
        elif kind == "call":
            row = by_id.get(e.get("attempt_id"))
            check(row is not None and row["call_seq"] == e["seq"] and not row["call_completed"]
                  and row["tool"] == e["tool"], "orphan/duplicate attempt measured call")
            row.update(call_completed=True, call_event=location,
                       measured_output_bytes=len(e["output"].encode()),
                       measured_output_sha256=sha(e["output"].encode()))
        elif kind == "attempt_end":
            row = by_id.get(e.get("attempt_id"))
            check(row is not None and not row["completed"], "duplicate/orphan attempt end")
            check(type(e["ended_ns"]) is int and e["ended_ns"] >= row["started_ns"], "invalid attempt duration")
            result, error = e["result"], e["protocol_error"]
            output = result["content"][0]["text"] if result is not None else None
            outcome = e["outcome"]
            check(outcome in {"ok", "failed", "timeout", "truncated", "refused", "protocol_error"},
                  "invalid attempt outcome")
            seq = e["call_seq"]
            check(seq is None or (type(seq) is int and seq > 0), "invalid attempt call sequence")
            check(seq == row["call_seq"], "attempt admission sequence differs")
            if seq is not None:
                c = call_rows.get(seq)
                s = call_starts.get(seq)
                check(c is not None and s is not None and c.get("attempt_id") == row["attempt_id"]
                      and s.get("attempt_id") == row["attempt_id"], "attempt/call binding differs")
                check(c["tool"] == row["tool"] and c["output"] == output and c["status"] == outcome,
                      "attempt/call output differs")
                check(type(c["elapsed_ms"]) is int and c["elapsed_ms"] >= 0
                      and c["elapsed_ms"] <= (e["ended_ns"] - row["started_ns"]) // 1_000_000,
                      "call/attempt duration differs")
                # Input truncation is a failed budget episode; never validate it
                # as a lossless input. All other calls must match exactly.
                full_input = broker.canonical(row["arguments"])
                check(c["input"] == full_input or
                      (c["status"] == "truncated" and full_input.startswith(c["input"])),
                      "attempt/call input differs")
            else:
                check(outcome in {"refused", "protocol_error"}, "completion missing measured call")
            if result is not None:
                check(type(result.get("isError")) is bool and result["isError"] == (outcome != "ok"),
                      "attempt result status differs")
                try:
                    body = json.loads(output)
                    error = body.get("error")
                except (ValueError, AttributeError):
                    error = None
            row.update(ended_ns=e["ended_ns"], elapsed_ns=e["ended_ns"] - row["started_ns"],
                       end_event=location, completed=True, successful=outcome == "ok", outcome=outcome,
                       observed_outcome=outcome,
                       error=error, call_seq=seq, output=output,
                       output_bytes=len(output.encode()) if output is not None else None,
                       output_sha256=sha(output.encode()) if output is not None else None)
            row["interruption_event"] = None
    check({e.get("attempt_id") for e in call_rows.values()} ==
          {r["attempt_id"] for r in ledger if r["call_completed"]}, "unaccounted completed call")
    check(len(call_rows) == len(calls), "call count differs")
    check({e["seq"] for e in requests.values() if e.get("method") == "tools/call"} ==
          {r["request_seq"] for r in ledger if r["request_seq"] is not None}, "unaccounted tools/call request")
    for row in ledger:
        reply = reply_rows.get(row["request_seq"])
        if reply is not None:
            check(row["completed"] and reply.get("attempt_id") == row["attempt_id"], "reply/attempt differs")
            row["reply_published"] = True
        else:
            row["successful"] = False
            row["outcome"] = "interrupted" if interrupted else "pending"
            row["interruption_event"] = interruption if interrupted else None
    def summary(selected):
        return {"attempts": len(selected), "responses": sum(r["completed"] for r in selected),
                "completed": sum(r["call_completed"] for r in selected),
                "successful": sum(r["successful"] for r in selected),
                "input_bytes": sum(r["input_bytes"] for r in selected),
                "output_bytes": sum(r["output_bytes"] or 0 for r in selected)}
    totals = summary(ledger)
    graph = summary([r for r in ledger if isinstance(r["tool"], str)
                     and (r["tool"].startswith("graph_") or r["tool"] in broker.GRAPH_TOOLS)])
    totals.update(graph_attempts=graph["attempts"], graph_completed=graph["completed"],
                  graph_successful=graph["successful"], all_attempt_denominator=None if coverage_issues else len(ledger),
                  coverage=dict(coverage, reported_attempts=len(ledger), expected_attempts=None if coverage_issues else len(ledger)),
                  by_tool={tool: summary([r for r in ledger if r["tool"] == tool]) for tool in
                           sorted({r["tool"] for r in ledger if isinstance(r["tool"], str)})})
    return ledger, totals


def reconcile(transcript, ledger, calls, runner):
    """Exact result reconciliation for calls AND refusals, with existing ID audit."""
    if any(not r["completed"] or not r["reply_published"] for r in ledger):
        return "incomplete broker tool attempt"
    # The original call rows still have to equal their corresponding attempts.
    measured = [{"tool": r["tool"], "input": broker.canonical(r["arguments"]),
                 "output": r["output"], "status": "ok" if r["observed_outcome"] == "ok" else "failed"}
                for r in ledger if r["completed"] and r["output"] is not None]
    if len([r for r in ledger if r["completed"] and r["call_seq"] is not None]) != len(calls):
        return "attempt ledger does not cover completed calls"
    mismatch = runner.reconcile_calls(transcript, measured)
    if mismatch:
        return mismatch
    # Remove exactly the refusal-result multiplicity, then apply the original
    # lossless completed-call reconciliation unchanged. A clipped measured input
    # must still fail that audit; the extra full-input diagnostic cannot waive it.
    refusals = Counter((r["tool"], runner.digest(r["arguments"]), "failed", r["output_sha256"])
                       for r in ledger if r["completed"] and r["call_seq"] is None
                       and r["output"] is not None)
    reported = Counter(transcript["mcp_call_audit"])
    reduced = dict(transcript, mcp_call_audit=list((reported - refusals).elements()))
    return runner.reconcile_calls(reduced, calls)


def timing(evidence, calls):
    check(set(evidence) == {"wall_start_ns", "provider_start_ns", "provider_end_ns", "preflight", "plugin_install"},
          "phase evidence fields differ")
    start, boundary, end = [evidence[k] for k in ("wall_start_ns", "provider_start_ns", "provider_end_ns")]
    check(all(type(n) is int and n >= 0 for n in (start, boundary, end)) and start <= boundary <= end,
          "invalid phase boundaries")
    wall, setup = (end - start) // 1_000_000, (boundary - start) // 1_000_000
    def span(record, within):
        check(set(record) == {"start_ns", "end_ns"} and all(type(n) is int for n in record.values())
              and 0 <= record["start_ns"] <= record["end_ns"], "invalid phase observation")
        check((start <= record["start_ns"] <= record["end_ns"] <= boundary) if within else
              record["end_ns"] <= start, "phase observation crosses its boundary")
        return (record["end_ns"] - record["start_ns"]) // 1_000_000
    preflight = evidence["preflight"]
    check(set(preflight) == {"outside_wall", "within_setup"}, "preflight phase fields differ")
    outside = span(preflight["outside_wall"], False)
    inside = span(preflight["within_setup"], True) if preflight["within_setup"] is not None else 0
    install = span(evidence["plugin_install"], True) if evidence["plugin_install"] is not None else 0
    return {"wall_ms": wall, "setup_ms": setup, "provider_ms": wall - setup,
            "overlapping": {"plugin_install_ms": install,
                            "graph_sync_ms": sum(c["elapsed_ms"] for c in calls
                                                 if c["tool"] in {"graph_sync", "graph_maintain"}),
                            "preflight_within_setup_ms": inside},
            "outside_wall": {"preflight_ms": outside, "source_export_ms": None,
                             "runtime_qualification_ms": None}, "end_to_end_ms": None}


def normalize(artifact, provider_raw, broker_raw, phase_evidence, runner):
    complete = (artifact["provider"]["exit"] == {"code": 0}
                and artifact["cleanup"]["terminated_by"] is None)
    observed = usage(provider_raw, artifact["files"]["provider.jsonl"]["truncated"], complete)
    observed["source"] = {"provider_binary_sha256": artifact["provider"]["binary_sha256"],
                          "provider_version": artifact["model"]["version"],
                          "format": "codex-exec-jsonl", "semantics_evidence": None}
    ledger, totals = attempts(broker_raw, artifact["calls"], artifact["files"]["broker-calls.jsonl"]["truncated"])
    transcript = runner.analyse_transcript(provider_raw, artifact["files"]["provider.jsonl"]["truncated"],
                                           artifact["request"]["tools"], prospective=True)
    mismatch = ("broker attempt coverage incomplete" if totals["coverage"]["state"] != "complete" else
                reconcile(transcript, ledger, artifact["calls"], runner))
    if mismatch is not None:
        # A prepared/published reply without exact receipt evidence cannot be
        # counted as successful graph use. Keep its observed producer outcome.
        for row in ledger:
            row["successful"] = False
        totals["successful"] = totals["graph_successful"] = 0
        for summary in totals["by_tool"].values():
            summary["successful"] = 0
    totals["provider_received_output_bytes"] = totals["output_bytes"] if mismatch is None else None
    provider_calls = []
    for event, location in rows(provider_raw):
        item = event.get("item") if event is not None else None
        if isinstance(item, dict) and item.get("type") == "mcp_tool_call":
            provider_calls.append({"event": location, "type": event.get("type"),
                                   "provider_call_id": item.get("id"), "server": item.get("server"),
                                   "tool": item.get("tool"), "status": item.get("status"),
                                   "error": item.get("error")})
    return {"contract": CONTRACT, "usage": observed, "attempts": ledger, "tools": totals,
            "provider_call_observations": provider_calls,
            "reconciliation": {"matched": mismatch is None, "error": mismatch},
            "timing": timing(phase_evidence, artifact["calls"])}


def legacy_usage(observed, source):
    values = {k: observed["fields"][k]["total"] for k in ("input_tokens", "output_tokens")}
    return dict(values, cost_usd=None, source=source) if any(v is not None for v in values.values()) else None


def verify(artifact, directory, runner):
    evidence = runner.load_json(directory / "phase-timing.json")
    expected = normalize(artifact, (directory / "provider.jsonl").read_bytes(),
                         (directory / "broker-calls.jsonl").read_bytes(), evidence, runner)
    check(runner.canonical(artifact.get("telemetry")) == runner.canonical(expected),
          "normalized ledger/coverage differs from capture")
    check(runner.canonical(artifact.get("usage")) == runner.canonical(legacy_usage(expected["usage"], runner.USAGE_SOURCE))
          and artifact.get("usage_raw") is None, "unsupported legacy aggregate")
    for key in ("wall_ms", "setup_ms", "provider_ms"):
        check(type(artifact["timing"][key]) is int and artifact["timing"][key] == expected["timing"][key], "phase sum differs")
    check(type(artifact["timing"]["graph_sync_ms"]) is int
          and artifact["timing"]["graph_sync_ms"] == expected["timing"]["overlapping"]["graph_sync_ms"],
          "sync sum differs")
    return expected
