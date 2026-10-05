"""Coverage-preserving source identities and separate attributed semantic judgments."""
from collections import Counter, defaultdict
import re
import secrets
import sys
from pathlib import Path


def api():
    # The CLI registers its imported dependencies once; tests import that same API.
    return sys.modules["effectiveness_source"].source, sys.modules["source_identity"]


def packet(body):
    source, _ = api()
    return source.seal(dict(schema_version=1, **body), "packet_sha256")


def truth_packets(corpus, lock):
    packets = []
    for c in corpus["cases"]:
        for n, item in enumerate(c["truth"]["identities"]):
            packets.append(packet(dict(kind="truth-identity", case_id=c["id"], target_id=f"i{n}",
                                       author=c["truth"]["author"], identity=item,
                                       source_universe=lock["snapshots"][c["head_view"]],
                                       source_evidence=item["context"], consequential=True)))
        for claim in c["truth"]["claims"]:
            packets.append(packet(dict(kind="truth-semantic", case_id=c["id"], target_id=claim["id"],
                                       author=c["truth"]["author"], claim=claim,
                                       source_evidence=claim["evidence"], consequential=True)))
        if "diff_review" in c:
            packets.append(packet(dict(kind="truth-diff", case_id=c["id"], target_id="historical-diff",
                                       author=c["truth"]["author"], diff=c["diff_review"],
                                       source_universes={view:lock["snapshots"][view] for view in (c["base_view"],c["head_view"])},
                                       source_evidence=[e for claim in c["truth"]["claims"] for e in claim["evidence"]], consequential=True)))
    return {"schema_version":1, "packets":packets, "source_manifests":lock["snapshots"],
            "absence_searches":{c["id"]:{**c["truth"]["absence_search"], "view":c["head_view"],
                                      "commit":lock["snapshots"][c["head_view"]]["git_revision"],
                                      "manifest_sha256":api()[0].digest(lock["snapshots"][c["head_view"]])}
                                for c in corpus["cases"] if "absence_search" in c["truth"]},
            "status":"independent-review-pending"}


def automated(root, files, selectors):
    """Never choose fewer files or Rust roots to get a pass from a bounded parser."""
    source, identity = api()
    py = sorted(p for p in files if p.endswith(".py"))
    roots = sorted(p for p in files if p.endswith(("/src/lib.rs", "/src/main.rs")) or p in ("src/lib.rs", "src/main.rs"))
    project = None
    failures = {}
    if len(py) > 128:
        failures["python"] = f"full Python universe has {len(py)} files; helper limit 128"
    if len(roots) != 1 and any(p.endswith(".rs") for p in files):
        failures["rust"] = f"full Rust universe has {len(roots)} crate roots; helper accepts one"
    if root is not None:
        project = identity.Project(root, rust_root=roots[0] if len(roots) == 1 else None,
                                   python_files=py if len(py) <= 128 else [])
        failures.update(project.failures)
    else:
        # Verification from Git is sufficient for authoring; automatic checks use exports.
        failures.update({lang:"automatic check requires full verified source export" for lang in ("python", "rust")})
    results = []
    for s in selectors:
        if project:
            syntax = project.check(s)
            if syntax["reason"] and any(x in syntax["reason"] for x in
                    ("malformed", "selector requires", "line must", "file path", "kind must", "name must", "unsupported language")):
                results.append(dict(outcome="contradicted", **syntax))
                continue
        if s["language"] in failures:
            results.append(dict(outcome="unsupported", identity_ok=None, citation_ok=None,
                                reason=failures[s["language"]], identity=None))
            continue
        checked = project.check(s)
        reason = checked["reason"] or ""
        outcome = "verified" if checked["identity_ok"] else "ambiguous" if "ambiguous" in reason else \
                  "unsupported" if any(x in reason for x in ("unsupported", "unresolved", "uncertain")) else "contradicted"
        results.append(dict(outcome=outcome, **checked))
    return {"items":results, "failures":failures, "pins":project.pins() if project else None}


def checked_reviews(document, packets, views, *, truth=False):
    source, _ = api()
    from_effectiveness = sys.modules.get("graph_effectiveness") or sys.modules["__main__"]
    source.shape(document, ["schema_version", "reviews", "adjudications"], "review document")
    source.require(document["schema_version"] == 1, "review schema")
    by_packet = {p["packet_sha256"]:p for p in packets}
    grouped = defaultdict(list)
    for review in [*document["reviews"], *document["adjudications"]]:
        source.shape(review, ["packet_sha256", "reviewer", "reviewed_at", "method", "blinding", "verdict",
                              "rationale", "answer_quote", "source_evidence", "identity", "initial_review_sha256", "review_sha256"], "review")
        source.check_seal(review, "review_sha256")
        p = by_packet.get(review["packet_sha256"])
        source.require(p is not None, "review not bound to a current packet")
        source.require(review["reviewer"] and review["reviewer"] != p.get("author"), "independent reviewer required")
        source.require(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", review["reviewed_at"]) and review["rationale"] and review["source_evidence"], "review attribution/evidence")
        source.require(review["blinding"] in ("source-only", "arm-blinded", "unblinded"), "honest blinding required")
        source.require(review["method"] == ("source-wide-independent" if p["kind"].endswith("identity") else "diff-source-review" if p["kind"] == "truth-diff" else "semantic-source-review"), "review method")
        source.require(review["verdict"] in ("verified", "contradicted", "ambiguous", "unsupported"), "review verdict")
        for e in review["source_evidence"]:
            from_effectiveness.evidence(e, views)
        source.require(all(e in review["source_evidence"] for e in p["source_evidence"]), "required source evidence omitted")
        if p["kind"].endswith("identity"):
            expected = p["identity"]
            source.require(set(review["identity"]) == {"owner", "module", "trait", "declaration"}, "source ownership review required")
            if review["verdict"] == "verified":
                selector = expected["selector"]
                lines = views[expected["view"]].get(selector["file"], "").splitlines()
                line = selector["line"]
                source.require(type(line) is int and 1 <= line <= len(lines) and
                               from_effectiveness.written_declaration(selector, lines[line-1]) and
                               selector["citation"]["quote"].strip() == lines[line-1].strip(),
                               "review contradicts written declaration/citation")
                source.require(review["identity"]["declaration"] == selector and review["identity"]["module"],
                               "source module/declaration review required")
                if expected.get("context_status") != "unresolved":
                    source.require(review["identity"] == {"owner":expected["owner"], "module":expected["module"],
                                    "trait":expected["trait"], "declaration":selector}, "review contradicts required/written identity")
                else:
                    source.require(any(e["view"] == expected["view"] and e["file"] == selector["file"] and
                                       e["start_line"] <= line <= e["end_line"] for e in review["source_evidence"]),
                                   "additional declaration source evidence required")
                    source.require(len(review["source_evidence"]) >= 2,
                                   "additional identity needs module/owner/trait source context as well as declaration")
        else:
            source.require(review["identity"] is None, "semantic review must stay separate")
        if not truth:
            quote = review["answer_quote"]
            source.require(isinstance(quote, str) and quote and quote in p["answer_text"], "review answer quote not in original answer")
        else:
            source.require(review["answer_quote"] is None, "truth review has no model answer")
        grouped[review["packet_sha256"]].append(review)
    decisions = {}
    for p in packets:
        key = p["packet_sha256"]
        initial = [r for r in grouped[key] if r in document["reviews"]]
        final = [r for r in grouped[key] if r in document["adjudications"]]
        source.require(len({r["reviewer"] for r in initial}) == len(initial) and len(initial) <= 2 and len(final) <= 1,
                       "duplicate reviewer or too many verdicts")
        source.require(all(not r["initial_review_sha256"] for r in initial), "initial reviews cannot claim adjudication")
        if not initial:
            source.require(not final, "adjudication without initial reviews")
            decisions[key] = "pending"
            continue
        consequential = len({r["verdict"] for r in initial}) > 1 or any(r["verdict"] == "ambiguous" for r in initial)
        if final:
            source.require(len(initial) == 2 and final[0]["reviewer"] not in {r["reviewer"] for r in initial}, "second independent review before adjudication")
            source.require(final[0]["initial_review_sha256"] == [r["review_sha256"] for r in initial], "both initial verdicts must be preserved")
            decisions[key] = final[0]["verdict"]
        else:
            decisions[key] = "pending" if consequential else initial[0]["verdict"]
    return decisions


def truth_admission(corpus, export_root, reviews, views):
    source, _ = api()
    ev = sys.modules.get("graph_effectiveness") or sys.modules["__main__"]
    lock = source.load(ev.HERE / "corpus.lock.json")
    packets = truth_packets(corpus, lock)["packets"]
    judgments = checked_reviews(reviews, packets, views, truth=True) if reviews else {}
    counts = Counter()
    admitted, pending = 0, []
    for c in corpus["cases"]:
        required = c["truth"]["identities"]
        checked = automated(Path(export_root) / "views" / c["head_view"] if export_root else None,
                            views[c["head_view"]], [x["selector"] for x in required])
        eligible = True
        for n, result in enumerate(checked["items"]):
            counts[result["outcome"]] += 1
            source.require(result["outcome"] not in ("ambiguous", "contradicted") and result["citation_ok"] is not False,
                           "invalid/ambiguous automatic required truth blocks admission")
            p = next(p for p in packets if p["case_id"] == c["id"] and p["kind"] == "truth-identity" and p["target_id"] == f"i{n}")
            judgment = judgments.get(p["packet_sha256"], "pending")
            source.require(judgment not in ("contradicted", "ambiguous"), "independent review contradicts required truth")
            counts["adjudicated_" + judgment] += 1
            # Root independent review remains required even when automation supports syntax.
            eligible &= judgment == "verified"
        for p in packets:
            if p["case_id"] == c["id"] and p["kind"] in ("truth-semantic", "truth-diff"):
                judgment = judgments.get(p["packet_sha256"], "pending")
                source.require(judgment not in ("contradicted", "ambiguous"), "contradictory required semantic truth")
                eligible &= judgment == "verified"
        if eligible:
            admitted += 1
        else:
            pending.append(c["id"])
    return {"admitted_cases":admitted, "total_cases":len(corpus["cases"]), "pending_cases":pending,
            "coverage":dict(counts), "live_admitted":False,
            "independent_diff_review":"verified" if all(judgments.get(p["packet_sha256"]) == "verified" for p in packets if p["kind"] == "truth-diff") else "pending"}


def selectors(answer, language):
    source, _ = api()
    kind_map = {"function":"fn" if language == "rust" else "function", "method":"fn" if language == "rust" else "function",
                "test":"fn" if language == "rust" else "function", "class":"struct" if language == "rust" else "class", "constant":"assignment"}
    result = []
    for item in answer["items"]:
        matches = [e for e in answer["evidence"] if e["item"] == item]
        source.require(len(matches) == 1, "one citation per submitted identity required")
        e = matches[0]
        path, sep, tail = item.removeprefix("symbol:").partition("#")
        name, colon, kind = tail.rpartition(":")
        source.require(item.startswith("symbol:") and sep and colon and kind in kind_map and path == e["file"], "answer selector/evidence shape")
        api()[1].canonical_path(path)
        # Runner quotes are trimmed; restore indentation from source only for comparison,
        # retaining the written quote separately. Never silently correct its content.
        result.append(dict(language=language, name=name, file=path, line=e["line"], kind=kind_map[kind],
                           citation={"start_line":e["line"], "end_line":e["line"], "quote":e["quote"]}))
    return result


def review_packets(corpus, plan, bundle, registry=None):
    source, _ = api()
    ev = sys.modules.get("graph_effectiveness") or sys.modules["__main__"]
    lock = source.load(ev.HERE / "corpus.lock.json")
    # The random tokens and bindings live in one frozen operator registry. They
    # cannot be enumerated from public arm/case/repetition identifiers.
    tokens = {}
    if registry is not None:
        source.require(registry["plan_sha256"] == plan["plan_sha256"] and
                       registry["bundle_sha256"] == source.digest(bundle), "review registry inputs changed")
        for binding in registry["operator_bindings"].values():
            order, token = binding["order"], binding["attempt_blind_id"]
            source.require(type(order) is int and re.fullmatch(r"[0-9a-f]{64}", token), "opaque review token")
            source.require(order not in tokens or tokens[order] == token, "inconsistent review token")
            tokens[order] = token
        source.require(len(set(tokens.values())) == len(tokens), "reused opaque attempt token")
    cases = {c["id"]:c for c in corpus["cases"]}
    packets, bindings = [], {}
    for record in bundle["records"]:
        if "episode" not in record or record["episode"]["status"] != "ok":
            continue
        slot = plan["slots"][record["order"]]
        c, answer = cases[slot["case_id"]], record["episode"]["answer"]
        if registry is not None:
            source.require(record["order"] in tokens, "missing review attempt binding")
        token = tokens.setdefault(record["order"], secrets.token_hex(32))
        text = source_text(answer)
        universe = {view:{"commit":lock["snapshots"][view]["git_revision"],
                          "tree":lock["snapshots"][view]["git_tree"],
                          "content_revision":lock["snapshots"][view]["content_revision"],
                          "manifest_sha256":source.digest(lock["snapshots"][view]),
                          "files":len(lock["snapshots"][view]["files"])} for view in (c["base_view"],c["head_view"])}
        for n, sel in enumerate(selectors(answer, c["language"])):
            # Required context is provided for matching frozen declarations; independent
            # source-wide reviewers must supply actual context for additional identities.
            target = next((x for x in c["truth"]["identities"] if all(x["selector"][k] == sel[k] for k in ("file", "line", "kind"))), None)
            written = dict(selector=sel, owner=target["owner"] if target else None, module=target["module"] if target else None,
                           trait=target["trait"] if target else None, view=c["head_view"],
                           context_status="frozen" if target else "unresolved")
            p = packet(dict(kind="answer-identity", case_id=c["id"], target_id=f"submitted-{n}", identity=written,
                            source_universes=universe, attempt_blind_id=token, answer_text=text, source_evidence=target["context"] if target else [], consequential=True))
            packets.append(p)
            bindings[p["packet_sha256"]] = {"order":record["order"], "target":f"submitted-{n}", "attempt_blind_id":token}
        for claim in c["truth"]["claims"]:
            p = packet(dict(kind="answer-semantic", case_id=c["id"], target_id=claim["id"], claim=claim,
                            source_universes=universe, attempt_blind_id=token, answer_text=text, source_evidence=claim["evidence"], consequential=True))
            packets.append(p)
            bindings[p["packet_sha256"]] = {"order":record["order"], "target":claim["id"], "attempt_blind_id":token}
    # Frozen random tokens also produce a stable random group presentation,
    # independent of the public schedule. Public list position cannot reveal it.
    packets.sort(key=lambda p:(p["attempt_blind_id"],p["target_id"],p["kind"]))
    result = {"schema_version":1, "plan_sha256":plan["plan_sha256"], "bundle_sha256":source.digest(bundle),
            "packets":packets, "operator_bindings":bindings,
            "blinding":"distribute packets only; operator bindings, arms, usage, tools and timings stay outside reviewer view; answer may reveal arm"}
    source.require(registry is None or result == registry, "review registry packets/bindings changed")
    return result


def source_text(answer):
    source, _ = api()
    return source.canonical(answer).decode()


def score(corpus, protocol, plan, bundle, export_root, views, reviews, truth_reviews, review_registry=None):
    source, _ = api()
    source.require(review_registry is not None or not (reviews["reviews"] or reviews.get("adjudications")),
                   "attributed answer reviews require the frozen operator --review-packets registry")
    review_set = review_packets(corpus, plan, bundle, review_registry)
    decisions = checked_reviews(reviews, review_set["packets"], views) if reviews["reviews"] or reviews.get("adjudications") else {}
    admission = truth_admission(corpus, export_root, truth_reviews, views)
    by_order = defaultdict(dict)
    for hash_, binding in review_set["operator_bindings"].items():
        by_order[binding["order"]][binding["target"]] = decisions.get(hash_, "pending")
    cases = {c["id"]:c for c in corpus["cases"]}
    rows = []
    for r in bundle["records"]:
        slot = plan["slots"][r["order"]]
        c = cases[slot["case_id"]]
        e = r.get("episode")
        row = dict(**slot, repository=c["repository"], family=c["family"], component=c["component"], status=e["status"] if e else r["status"],
                   error=e["error"] if e else r["error"], correct=False, semantic="not-applicable", identities=[],
                   identity_recall={"matched":0,"required":len(c["truth"]["identities"])}, citation_recall={"matched":0,"required":len(c["truth"]["identities"])},
                   calls=len(e["calls"]) if e else 0, graph_calls=0, successful_graph_calls=0,
                   output_bytes=e["output_bytes"] if e else None, setup_output_bytes=e["setup_output_bytes"] if e else None,
                   timing=e["timing"] if e else {"wall_ms":r["elapsed_ms"],"setup_ms":r["elapsed_ms"],"provider_ms":None,"graph_sync_ms":None},
                   usage=observed_usage(e) if e else None,
                   telemetry=e.get("telemetry") if e else None,
                   review_attribution=[review for review in [*reviews["reviews"], *reviews["adjudications"]]
                                       if review_set["operator_bindings"][review["packet_sha256"]]["order"] == r["order"]])
        row["truth_status"] = "pending" if c["id"] in admission["pending_cases"] else "verified"
        if e:
            gc = [call for call in e["calls"] if call["tool"].startswith("graph_")]
            row.update(graph_calls=len(gc), successful_graph_calls=sum(call["status"] == "ok" for call in gc))
            if e.get("telemetry"):
                tools = e["telemetry"]["tools"]
                row.update(calls=tools["attempts"], graph_calls=tools["graph_attempts"],
                           successful_graph_calls=tools["graph_successful"],
                           call_denominator=tools["all_attempt_denominator"], tool_coverage=tools["coverage"],
                           tool_attempts=e["telemetry"]["attempts"], tool_accounting=tools,
                           phase_timing=e["telemetry"]["timing"])
            else:
                row.update(call_denominator=row["calls"],tool_coverage=None,tool_attempts=None,tool_accounting=None,phase_timing=None)
        if e and e["status"] == "ok":
            answer = e["answer"]
            submitted = selectors(answer, c["language"])
            # Reconstitute indentation required by source-identity-v1, but only
            # after independently checking the runner's whole trimmed line.
            valid_quotes = []
            for sel in submitted:
                lines = views[c["head_view"]].get(sel["file"], "").splitlines()
                line = sel["line"]
                ok = type(line) is int and 1 <= line <= len(lines) and lines[line-1].strip() == sel["citation"]["quote"]
                valid_quotes.append(ok)
                if ok:
                    sel["citation"]["quote"] = lines[line-1]
            check = automated(Path(export_root)/"views"/c["head_view"], views[c["head_view"]], submitted)
            found, cited = set(), set()
            additional_pending, wrong = False, False
            for n, (sel, auto) in enumerate(zip(submitted, check["items"])):
                judgment = by_order[r["order"]].get(f"submitted-{n}", "pending")
                verified = auto["outcome"] != "contradicted" and judgment == "verified"
                if auto["outcome"] == "contradicted" or judgment == "contradicted":
                    wrong = True
                candidates = [i for i,x in enumerate(c["truth"]["identities"]) if all(x["selector"][k] == sel[k] for k in ("file","line","kind"))]
                # Name equivalence comes only from verified automation or independent review.
                if candidates and verified:
                    found.add(candidates[0])
                    if valid_quotes[n] and (auto["citation_ok"] is not False):
                        cited.add(candidates[0])
                elif not candidates and not verified:
                    additional_pending = True
                row["identities"].append(dict(selector=sel, automated=auto, adjudicated=judgment,
                                               exact_source_quote=valid_quotes[n], effective="verified" if verified else judgment))
            semantic_verdicts = [by_order[r["order"]].get(claim["id"], "pending") for claim in c["truth"]["claims"]]
            row["semantic"] = "verified" if all(x == "verified" for x in semantic_verdicts) else "contradicted" if "contradicted" in semantic_verdicts else "pending"
            row["identity_recall"]["matched"], row["citation_recall"]["matched"] = len(found), len(cited)
            pending = any(x["adjudicated"] in ("pending", "ambiguous", "unsupported") for x in row["identities"])
            objective = len(cited) == len(c["truth"]["identities"]) and not wrong and not additional_pending
            if not submitted and c["truth"]["identities"]:
                row["correct"] = False
            elif wrong or row["semantic"] == "contradicted" or any(not x for x in valid_quotes):
                row["correct"] = False
            elif pending or row["semantic"] == "pending":
                row["correct"] = None
            else:
                row["correct"] = objective and row["semantic"] == "verified" and answer["abstain"] == (not c["truth"]["identities"])
            if row["truth_status"] == "pending" and row["correct"] is True:
                row["correct"] = None
        rows.append(row)
    import analysis
    return analysis.report(rows, admission, protocol, plan, bundle["study_kind"])


def observed_usage(episode):
    """Only qualified schema-3 per-field totals enter efficiency estimates."""
    source, _ = api()
    if episode.get("telemetry") is not None:
        telemetry = episode["telemetry"]
        source.require(telemetry["contract"] == "prospective-accounting-v1", "telemetry contract")
        return {**{key: value["total"] for key, value in telemetry["usage"]["fields"].items()},
                "cost_usd":telemetry["usage"].get("cost_usd")}
    # Explicit scorer fixtures are synthetic diagnostics, never live accounting.
    return sys.modules["effectiveness_source"].observed_usage(episode)
