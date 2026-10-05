"""Source-truth scoring. No candidate, extractor, or database imports."""
from collections import Counter
import json
import math


MISSING = object()
CONFIDENCE = {"fuzzy_name": 0, "same_module": 1, "import_resolved": 2, "exact": 3}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def select(value, path):
    """Dot paths; * flattens one array level. Absent differs from JSON null."""
    parts = path.split(".") if path else []
    if not parts:
        return value
    first, rest = parts[0], ".".join(parts[1:])
    if first == "*":
        if not isinstance(value, list):
            return MISSING
        result = []
        for item in value:
            found = select(item, rest)
            if found is MISSING:
                return MISSING
            result.extend(found if isinstance(found, list) else [found])
        return result
    if isinstance(value, dict):
        child = value.get(first, MISSING)
    elif isinstance(value, list) and first.isdecimal() and int(first) < len(value):
        child = value[int(first)]
    else:
        child = MISSING
    return select(child, rest) if child is not MISSING else MISSING


def project(rows, fields):
    if not isinstance(rows, list):
        raise ValueError("expected an array")
    projected = []
    for row in rows:
        values = [select(row, field) for field in fields]
        # Optional absent base/head side is explicitly null, never an unknown identity.
        for i, field in enumerate(fields):
            if values[i] is MISSING and field in ("base.selector", "head.selector"):
                side = row.get(field.split(".")[0], MISSING)
                if side is None:
                    values[i] = None
        if any(value is MISSING for value in values):
            raise ValueError("missing projected field")
        projected.append(canonical(values))
    return Counter(projected)


def ambiguity_disclosed(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key in ("ambiguous", "ambiguous_selection") and child is True:
                return True
            if key in ("candidates", "uncertain_candidates") and isinstance(child, list) and len(child) > 1:
                return True
            if key == "code" and child in ("ambiguous", "ambiguous_selector", "ambiguous_command"):
                return True
            if isinstance(child, (dict, list)) and ambiguity_disclosed(child):
                return True
    elif isinstance(value, list):
        return any(ambiguity_disclosed(child) for child in value)
    return False


def uncertain_identities(rows, expected):
    """Cover occurrences without inventing correspondence between changed names."""
    if not isinstance(rows, list) or not rows:
        return False
    actual = []
    for row in rows:
        status = row.get("status")
        base, head = row.get("base", MISSING), row.get("head", MISSING)
        candidates = row.get("uncertain_candidates", MISSING)
        if status == "uncertain":
            if (row.get("pairing") != "uncertain" or not row.get("note")
                    or not isinstance(candidates, list) or len(candidates) < 2):
                return False
            occurrences = candidates
            if any(not candidate.get("reason") for candidate in candidates):
                return False
            # Optional anchored sides must be represented among the same candidates.
            for side, occurrence in (("base", base), ("head", head)):
                if occurrence is MISSING:
                    return False
                if occurrence is not None and (occurrence.get("snapshot") != side or
                        project([occurrence], ["snapshot", "selector", "commit_sha"]) -
                        project(candidates, ["snapshot", "selector", "commit_sha"])):
                    return False
        elif status in ("added", "removed"):
            side, occurrence, absent = ("head", head, base) if status == "added" else ("base", base, head)
            if (not isinstance(occurrence, dict) or occurrence.get("snapshot") != side
                    or absent is not None or candidates != [] or row.get("pairing") != "same_selector"):
                return False
            occurrences = [occurrence]
        else:
            return False
        actual.extend(occurrences)
    return project(actual, ["snapshot", "selector", "commit_sha"]) == Counter(canonical(row) for row in expected)


def ranking_metrics(rows, check):
    """Independent fixture counts plus the published metric formulas, not scores."""
    fields = ["variant", "level", "k", "cases", "relevant", "true_positives", "returned"]
    if project(rows, fields) != Counter(canonical(row) for row in check["value"]):
        return False
    for row in rows:
        tp, relevant, returned, cases, k = (row[key] for key in
                                           ("true_positives", "relevant", "returned", "cases", "k"))
        if any(type(row[key]) is not int or row[key] < 0 for key in fields[2:]):
            return False
        if tp > min(relevant, returned, cases * k):
            return False
        expected = {"recall_at_k": tp / relevant if relevant else None,
                    "precision_at_k": tp / (cases * k) if cases * k else None,
                    "stale_result_rate": 0.0 if returned else None}
        for key, truth in expected.items():
            value = row.get(key, MISSING)
            if truth is None:
                if value is not None:
                    return False
            elif (type(value) not in (int, float) or not math.isfinite(value)
                  or not 0 <= value <= 1 or not math.isclose(value, truth, rel_tol=1e-12)):
                return False
        mean, maximum = row.get("mean_latency_ms"), row.get("max_latency_ms")
        if any(type(v) not in (int, float) or not math.isfinite(v) or v < 0 for v in (mean, maximum)) or mean > maximum:
            return False
    return True


def constraint(check, payload):
    value = select(payload, check["path"])
    op = check["op"]
    if value is MISSING:
        raise ValueError("missing field " + check["path"])
    metrics = None
    if op == "exists":
        good = True
    elif op == "eq":
        good = canonical(value) == canonical(check["value"])
    elif op == "set":
        actual = project(value, check["fields"])
        expected = Counter(canonical(row) for row in check["value"])
        good = actual == expected
        if check.get("complete"):
            tp = sum((actual & expected).values())
            returned, eligible = sum(actual.values()), sum(expected.values())
            metrics = {"unit": check["path"], "true_positives": tp, "returned": returned,
                       "eligible": eligible, "precision": tp / returned if returned else None,
                       "recall": tp / eligible if eligible else None,
                       "false_positives": sum((actual - expected).values()),
                       "false_negatives": sum((expected - actual).values())}
    elif op == "confidence_ceiling":
        good = isinstance(value, list)
        for edge in value if good else []:
            if edge.get("target_name") in check["names"]:
                good &= (edge.get("confidence") in CONFIDENCE
                         and CONFIDENCE[edge["confidence"]] <= CONFIDENCE[check["value"]]
                         and edge.get("target_qualified", MISSING) is None)
            else:
                good = False
        # Empty edges represent an omission, never a measured true positive.
    elif op == "forbid_target":
        good = isinstance(value, list) and all(
            edge.get("target_name") != check["value"] for edge in value)
    elif op == "uncertain_pairing":
        good = uncertain_identities(value, check["value"])
    elif op == "changes_provenance":
        good = isinstance(value, list)
        for row in value if good else []:
            for side in ("base", "head"):
                occurrence = row.get(side, MISSING)
                good &= occurrence is None or isinstance(occurrence, dict) and occurrence.get("snapshot") == side and occurrence.get("commit_sha") == check[side]
            for occurrence in row.get("uncertain_candidates", []):
                side = occurrence.get("snapshot")
                good &= side in ("base", "head") and occurrence.get("commit_sha") == check.get(side)
    elif op == "live_selectors":
        good = isinstance(value, list) and bool(value)
        good &= all(row.get("selector") in check["value"] for row in value)
    elif op == "budget_truth":
        good = (value.get("complete") is True and value.get("state") == "published"
                or value.get("complete") is False and value.get("state") == "budget_exhausted"
                and bool(value.get("note")) and bool(value.get("phase")))
    elif op == "changes_budget_truth":
        incomplete = value.get("incomplete")
        good = (value.get("complete") is True and incomplete is None
                or value.get("complete") is False and isinstance(incomplete, dict)
                and incomplete.get("phase") in ("indexing", "analysis")
                and incomplete.get("bound") == "budget_ms" and incomplete.get("value") == 1000
                and bool(incomplete.get("message")))
        good &= select(value, "query.bounds.budget_ms") == 1000
        good &= select(value, "query.bounds.query_budget_ms") == 100
        good &= select(value, "query.bounds.node_cap") == 1
        if isinstance(incomplete, dict) and incomplete.get("phase") == "indexing":
            good &= value.get("comparison") is None and value.get("symbols") == []
        else:
            good &= select(value, "comparison.base.commit_sha") == check["base"]
            good &= select(value, "comparison.head.commit_sha") == check["head"]
            analysed = value.get("symbols", [])
            omitted = value.get("not_analysed", [])
            identities = [row.get("selector") for row in analysed + omitted]
            good &= Counter(identities) == Counter(check["value"])
            good &= value.get("truncated") is True
        flags = value.get("truncation")
        good &= isinstance(flags, list) and bool(flags) and all(
            flag.get("bound") and type(flag.get("value")) is int and flag["value"] > 0
            and flag.get("what") for flag in flags)
    elif op == "ranking_metrics":
        good = ranking_metrics(value, check)
    elif op == "truth_omissions":
        good = (value.get("file_changes_total") == 2 and value.get("file_truth_eligible") == 1
                and sum(value.get("file_truth_omitted", {}).values()) == 1
                and value.get("symbol_truth_eligible") == 1
                and sum(value.get("symbol_truth_omitted", {}).values()) == 1)
    elif op == "null_metrics":
        good = isinstance(value, list) and len(value) == 8 and all(
            row.get("cases") == 0 and all(row.get(key, MISSING) is None for key in
            ("recall_at_k", "precision_at_k", "stale_result_rate", "mean_latency_ms", "max_latency_ms"))
            for row in value)
    else:
        raise ValueError("unknown scoring operation " + op)
    return bool(good), metrics


def score(case, payload, error_code=None, transport_ok=True):
    findings, metrics, observations = [], [], []
    # The installed recommendation wraps the same result the standalone CLI emits.
    if (case.get("tool") == "recommend" and not case.get("action")
            and isinstance(payload, dict) and isinstance(payload.get("result"), dict)):
        payload = payload["result"]
    if not transport_ok:
        findings.append("transport failed, timed out, truncated, or left descendants")
    if case.get("expected_error"):
        if error_code != case["expected_error"]:
            findings.append("expected " + case["expected_error"] + "; got " + str(error_code))
    elif error_code is not None or not isinstance(payload, dict):
        findings.append("unexpected product error: " + str(error_code))
    else:
        for check in case["checks"]:
            try:
                passed, measure = constraint(check, payload)
                if measure is not None:
                    metrics.append(measure)
                if check["op"] == "confidence_ceiling":
                    edges = select(payload, check["path"])
                    if isinstance(edges, list):
                        observations.append({"unit": "written_call", "written": case["coverage"]["unresolved"],
                                             "returned_uncertain": len(edges),
                                             "omitted": max(0, case["coverage"]["unresolved"] - len(edges)),
                                             "precision": None, "recall": None})
                if not passed:
                    findings.append(check["op"] + " failed at " + check["path"])
            except (ValueError, TypeError, KeyError, AttributeError) as error:
                findings.append(str(error))
        # Do not convert an ambiguous short name into exact identity coverage.
        challenge = case.get("challenge")
        if challenge and not ambiguity_disclosed(payload):
            findings.append(challenge + ": ambiguity is undisclosed")
    return {"passed": not findings, "findings": findings, "metrics": metrics, "coverage_observations": observations,
            "declared_coverage": case["coverage"]}


def aggregate(rows, tools):
    result = {}
    for tool in tools:
        selected = [row for row in rows if row["tool"] == tool]
        result[tool] = {"passed": bool(selected) and all(row["score"]["passed"] for row in selected),
                        "samples": len(selected), "failures": sum(not row["score"]["passed"] for row in selected),
                        "output_bytes": sum(row["output_bytes"] for row in selected),
                        "latency_ms": [row["latency_ms"] for row in selected],
                        "cases": sorted({row["case_id"] for row in selected}),
                        "source_truth_metrics": [{"case_id": row["case_id"], "surface": row["surface"],
                                                  "metrics": row["score"]["metrics"]}
                                                 for row in selected if row.get("sample") == 0],
                        "cost_samples": [{"case_id": row["case_id"], "surface": row["surface"],
                                          "state": row.get("state"), "sample": row.get("sample"),
                                          "output_bytes": row["output_bytes"], "latency_ms": row["latency_ms"]}
                                         for row in selected]}
    return result
