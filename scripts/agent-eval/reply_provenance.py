"""Prospective safe tool text contract; no credentials or raw replies are persisted here.

Commitments attest what the pinned producer observed, not independent proof of
secret preimages. Replay verifies their consistency against the separately sealed
broker log, source binding, provider results and byte costs.
"""
import hashlib
import json
import re

CONTRACT = "safe-tool-text-v1"
MARKER = "[REDACTED]"
SHAPES = (
    re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}"),
    re.compile(r"(?<![A-Za-z0-9_-])sk-[A-Za-z0-9_-]{16,}"),
    re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
)
HEX = re.compile(r"[0-9a-f]{64}\Z")


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def digest(value):
    return sha(json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False))


def check(condition, message):
    if not condition:
        raise ValueError("reply provenance: " + message)


class Redactor:
    """Shared durable/delivery policy, including escaped forms of host values.

    The broker receives only salted fingerprints and character lengths. It
    never receives credential values through configuration, argv or child env.
    These commitments remain private evidence, not publishable anonymization.
    """

    def __init__(self, values=(), policy=None):
        if policy is None:
            variants = set()
            for value in values:
                if len(value) < 8:
                    continue
                variants.add(value)
                for _ in range(4):  # nested product JSON, MCP text and JSONL
                    value = json.dumps(value, ensure_ascii=True)[1:-1]
                    variants.add(value)
            # A deterministic empty policy permits comparison across both arms.
            import secrets
            salt = secrets.token_hex(16) if variants else ""
            policy = {"contract": CONTRACT, "salt": salt, "values": sorted(
                [{"chars": len(v), "sha256": sha(salt + v)} for v in variants],
                key=lambda v: (v["chars"], v["sha256"]))}
        check(isinstance(policy, dict) and set(policy) == {"contract", "salt", "values"}
              and policy["contract"] == CONTRACT and isinstance(policy["salt"], str)
              and isinstance(policy["values"], list), "invalid policy")
        self.policy = policy
        self.matchers = {}
        for item in policy["values"]:
            check(isinstance(item, dict) and set(item) == {"chars", "sha256"}
                  and type(item["chars"]) is int and item["chars"] >= 8
                  and isinstance(item["sha256"], str) and HEX.fullmatch(item["sha256"]),
                  "invalid value fingerprint")
            self.matchers.setdefault(item["chars"], set()).add(item["sha256"])

    def transform(self, text):
        matches = []
        for index, pattern in enumerate(SHAPES):
            matches.extend((m.start(), m.end(), "shape-" + str(index))
                           for m in pattern.finditer(text))
        for length, hashes in self.matchers.items():
            for start in range(len(text) - length + 1):
                if sha(self.policy["salt"] + text[start:start + length]) in hashes:
                    matches.append((start, start + length, "host-value"))
        # Union overlaps: neither a nested shape nor a longer host value leaks.
        merged = []
        for start, end, rule in sorted(matches):
            if merged and start < merged[-1][1]:
                old = merged[-1]
                merged[-1] = (old[0], max(end, old[1]), sorted(set(old[2] + [rule])))
            else:
                merged.append((start, end, [rule]))
        pieces, spans, cursor, size = [], [], 0, 0
        for start, end, rules in merged:
            prefix = text[cursor:start]
            size += len(prefix.encode())
            spans.append({"start": size, "original_bytes": len(text[start:end].encode()),
                          "rules": rules})
            pieces.extend((prefix, MARKER))
            size += len(MARKER)
            cursor = end
        pieces.append(text[cursor:])
        safe = "".join(pieces)
        return safe, {"original_sha256": sha(text), "original_bytes": len(text.encode()),
                      "safe_sha256": sha(safe), "safe_bytes": len(safe.encode()),
                      "spans": spans}

    def text(self, text):
        safe, proof = self.transform(text)
        return safe, len(proof["spans"])

    def value(self, value):
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            pairs = [self.value(item) for item in value]
            return [item for item, _ in pairs], sum(count for _, count in pairs)
        if isinstance(value, dict):
            result, total = {}, 0
            for key, item in value.items():
                key, count = self.text(key)
                total += count
                item, count = self.value(item)
                total += count
                check(key not in result, "redaction collides with an object key")
                result[key] = item
            return result, total
        return value, 0


def verify_text(safe, proof, policy):
    check(isinstance(proof, dict) and set(proof) == {
        "original_sha256", "original_bytes", "safe_sha256", "safe_bytes", "spans"}, "text fields")
    check(type(proof["original_bytes"]) is int and proof["original_bytes"] >= 0
          and type(proof["safe_bytes"]) is int and proof["safe_bytes"] == len(safe.encode())
          and proof["safe_sha256"] == sha(safe)
          and isinstance(proof["original_sha256"], str)
          and HEX.fullmatch(proof["original_sha256"]), "text digest/size differs")
    check(isinstance(proof["spans"], list), "invalid transformation spans")
    data, end, delta = safe.encode(), 0, 0
    for span in proof["spans"]:
        check(isinstance(span, dict) and set(span) == {"start", "original_bytes", "rules"}
              and type(span["start"]) is int and span["start"] >= end
              and type(span["original_bytes"]) is int and span["original_bytes"] >= 8
              and isinstance(span["rules"], list) and span["rules"]
              and span["rules"] == sorted(set(span["rules"]))
              and all(rule in {"shape-0", "shape-1", "shape-2", "host-value"}
                      for rule in span["rules"]), "invalid transformation span")
        check("host-value" not in span["rules"] or policy["values"], "missing host policy")
        end = span["start"] + len(MARKER)
        check(data[span["start"]:end] == MARKER.encode(), "masked span differs")
        delta += span["original_bytes"] - len(MARKER)
    check(proof["original_bytes"] == len(data) + delta, "transformation cost differs")
    check(bool(proof["spans"]) == (proof["original_sha256"] != proof["safe_sha256"]),
          "transformation digest differs")
    check(Redactor(policy=policy).text(safe)[1] == 0, "unresolved credential in safe text")


def source_binding(request, inputs, provenance):
    return digest({"request_digest": digest(request), "inputs": inputs,
                   "source_provenance": provenance})


def validate_contract(contract):
    check(isinstance(contract, dict) and set(contract) == {"version", "policy", "source_binding"}
          and contract["version"] == CONTRACT and isinstance(contract["source_binding"], str)
          and HEX.fullmatch(contract["source_binding"]), "unsupported contract")
    Redactor(policy=contract["policy"])


def verify_capture(artifact, log):
    """No reinterpretation of historical captures, including historical refusals."""
    if artifact["runner_version"] != "5":
        check("reply_contract" not in artifact and "reply_provenance" not in artifact
              and not log["reply_contracts"] and not log["reply_provenance"],
              "prospective evidence under a historical runner")
        return
    check(set(artifact["harness"]) == {"eval_runner.py", "eval_broker.py", "plugin_profile.py",
                                      "reply_provenance.py"}
          and all(isinstance(v, str) and HEX.fullmatch(v) for v in artifact["harness"].values())
          and artifact["harness"]["eval_broker.py"] == artifact["isolation"]["broker_sha256"],
          "harness identity differs")
    contract = artifact.get("reply_contract")
    validate_contract(contract)
    check(contract["source_binding"] == source_binding(
        artifact["request"], artifact["inputs"], artifact["source_provenance"]), "source binding differs")
    check(log["reply_contracts"] == [contract] * len(log["starts"]), "broker contract differs")
    check(log["audit_redactions"] == 0, "unresolved broker audit redaction")
    proofs = artifact.get("reply_provenance")
    check(isinstance(proofs, list) and proofs == log["reply_provenance"]
          and len(proofs) == len(artifact["calls"]), "call proof/log differs")
    for seq, (call, proof) in enumerate(zip(artifact["calls"], proofs), 1):
        check(isinstance(proof, dict) and set(proof) == {
            "seq", "contract_sha256", "text", "safe_output", "delivered_sha256",
            "delivered_bytes", "truncated", "call_sha256", "views"}, "call proof fields")
        check(type(proof["seq"]) is int and proof["seq"] == seq
              and proof["contract_sha256"] == digest(contract)
              and proof["call_sha256"] == digest(call), "call/contract binding differs")
        check(isinstance(proof["views"], list), "invalid source views")
        for view in proof["views"]:
            check(isinstance(view, dict) and set(view) == {"role", "text", "safe_text"}
                  and view["role"] in {"read-line", "page-input", "plugin-result"},
                  "invalid source view")
            verify_text(view["safe_text"], view["text"], contract["policy"])
        verify_text(proof["safe_output"], proof["text"], contract["policy"])
        view = json.loads(proof["safe_output"]).get("text_view")
        check(view == {"contract": CONTRACT, "source_metadata": "original",
                       "redacted": bool(proof["views"] or proof["text"]["spans"])},
              "provider transformation disclosure differs")
        output = call["output"]
        check(type(proof["delivered_bytes"]) is int
              and proof["delivered_bytes"] == len(output.encode())
              and proof["delivered_sha256"] == sha(output)
              and type(proof["truncated"]) is bool, "delivered cost/hash differs")
        if proof["truncated"]:
            check(call["status"] == "truncated" and proof["safe_output"].startswith(output)
                  and len(output.encode()) < proof["text"]["safe_bytes"], "truncation differs")
        else:
            check(output == proof["safe_output"], "delivered text differs")
