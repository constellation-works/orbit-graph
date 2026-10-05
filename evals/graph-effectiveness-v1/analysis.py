"""Descriptive clustered paired estimates and prospective precision planning; no claim gate."""
from collections import Counter, defaultdict
import math
import random
from statistics import NormalDist, mean, variance


def interval(values, alpha=0.05, seed=14002):
    if len(values) < 8 or len(set(values)) < 2:
        return None
    rng = random.Random(seed)  # Simulation only; never tokens or identity.
    samples = sorted(mean(rng.choices(values, k=len(values))) for _ in range(4000))
    return [samples[int(alpha / 2 * len(samples))], samples[min(len(samples)-1, int((1-alpha/2)*len(samples)))]]


TOKEN_FIELDS = ("input_tokens", "output_tokens", "cached_input_tokens", "reasoning_tokens", "total_tokens")


def tokens(row, field="total_tokens"):
    """A nullable qualified field, never a sum of other counters or diagnostics."""
    value = (row["usage"] or {}).get(field)
    return value if type(value) is int and value >= 0 else None


def paired(rows):
    pairs = defaultdict(dict)
    for r in rows:
        pairs[(r["case_id"], r["repetition"])][r["arm"]] = r
    quality, both_time = defaultdict(list), []
    token_delta = {field:defaultdict(list) for field in TOKEN_FIELDS}
    reductions = {field:defaultdict(list) for field in TOKEN_FIELDS}
    discordant = Counter()
    for (case, repetition), pair in pairs.items():
        if set(pair) != {"baseline", "graph"}:
            raise ValueError("paired denominator missing an arm")
        b, g = pair["baseline"], pair["graph"]
        if b["correct"] is not None and g["correct"] is not None:
            quality[case].append(int(g["correct"]) - int(b["correct"]))
            discordant["graph_only" if g["correct"] and not b["correct"] else "baseline_only" if b["correct"] and not g["correct"] else "concordant"] += 1
        for field in TOKEN_FIELDS:
            bt, gt = tokens(b, field), tokens(g, field)
            if bt is not None and gt is not None:
                token_delta[field][case].append(gt-bt)
                if bt > 0:
                    reductions[field][case].append(1-gt/bt)
        if b["correct"] is True and g["correct"] is True:
            both_time.append(dict(case_id=case,repetition=repetition,graph_minus_baseline_wall_ms=g["timing"]["wall_ms"]-b["timing"]["wall_ms"]))
    # All repetitions must contribute to a case before its estimate enters uncertainty.
    repeated = Counter(case for case, _ in pairs)
    quality_means = {k:mean(v) for k,v in quality.items() if len(v)==repeated[k]}
    comparisons = {}
    for field in TOKEN_FIELDS:
        delta_means = {k:mean(v) for k,v in token_delta[field].items() if len(v)==repeated[k]}
        reduction_means = {k:mean(v) for k,v in reductions[field].items() if len(v)==repeated[k]}
        observed_pairs = sum(len(v) for v in token_delta[field].values())
        comparisons[field] = dict(graph_minus_baseline=mean(delta_means.values()) if delta_means else None,
                                  per_case_delta=delta_means, observed_cases=len(delta_means), total_cases=len(repeated),
                                  observed_pairs=observed_pairs, total_pairs=len(pairs), missing_pairs=len(pairs)-observed_pairs,
                                  reduction=mean(reduction_means.values()) if reduction_means else None,
                                  reduction_ci=interval(list(reduction_means.values())), observed_reduction_cases=len(reduction_means),
                                  reduction_pairs=sum(len(v) for v in reductions[field].values()),
                                  qualification="independently qualified field totals only; diagnostic observed_sum excluded")
    total = comparisons["total_tokens"]
    return dict(total_pairs=len(pairs), total_cases=len(repeated), complete_quality_cases=len(quality_means),
                per_case_quality_delta=quality_means, accuracy_delta=mean(quality_means.values()) if quality_means else None,
                descriptive_case_bootstrap_ci=interval(list(quality_means.values())),
                discordance=dict(discordant), token_fields=comparisons, paired_token_delta=total["graph_minus_baseline"],
                observed_token_cases=total["observed_cases"], token_reduction=total["reduction"],
                token_reduction_ci=total["reduction_ci"], observed_reduction_cases=total["observed_reduction_cases"],
                both_correct_time={"conditional":True,"pairs":len(both_time),"all_pairs":len(pairs),"rows":both_time},
                missing_quality_pairs=len(pairs)-sum(len(v) for v in quality.values()),
                missing_token_pairs=total["missing_pairs"])


def report(rows, admission, protocol, plan, kind):
    arms = {}
    for arm in ("baseline", "graph"):
        selected = [r for r in rows if r["arm"] == arm]
        correct = sum(r["correct"] is True for r in selected)
        pending = sum(r["correct"] is None for r in selected)
        fields = ("input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens", "total_tokens", "cost_usd")
        usage = {}
        for field in fields:
            observed = [(r["usage"] or {}).get(field) for r in selected]
            observed = [x for x in observed if x is not None]
            usage[field] = dict(sum=sum(observed) if observed else None, observed=len(observed), total=len(selected), missing=len(selected)-len(observed))
            usage[field]["field_coverage"] = [r["telemetry"]["usage"]["fields"].get(field) if r.get("telemetry") else None for r in selected]
        automatic = Counter(x["automated"]["outcome"] for r in selected for x in r["identities"])
        adjudicated = Counter(x["adjudicated"] for r in selected for x in r["identities"])
        successes = [r for r in selected if r["status"] == "ok"]
        adoption = [True if r["graph_calls"] > 0 else None if r.get("tool_coverage") and
                    r["tool_coverage"]["state"] != "complete" else False for r in selected]
        arms[arm] = dict(attempts=len(selected), correct=correct, incorrect=len(selected)-correct-pending, pending=pending,
                         accuracy=correct/len(selected) if not pending else None,
                         accuracy_bounds=[correct/len(selected),(correct+pending)/len(selected)],
                         outcomes=dict(Counter(r["status"] for r in selected)), automated_coverage=dict(automatic),
                         adjudicated_coverage=dict(adjudicated), semantic_outcomes=dict(Counter(r["semantic"] for r in selected)),
                         usage=usage, calls=sum(r["calls"] for r in selected), graph_calls=sum(r["graph_calls"] for r in selected),
                         successful_graph_calls=sum(r["successful_graph_calls"] for r in selected),
                         adoption_all=sum(x is True for x in adoption)/len(selected) if None not in adoption else None,
                         adoption_bounds=[sum(x is True for x in adoption)/len(selected),
                                          sum(x is not False for x in adoption)/len(selected)],
                         tool_coverage=[r.get("tool_coverage") for r in selected],
                         adoption_successful=sum(r["graph_calls"]>0 for r in successes)/len(successes) if successes else None,
                         successful_attempts=len(successes),
                         bytes={key:dict(sum=sum(r[key] for r in selected if r[key] is not None) if any(r[key] is not None for r in selected) else None,
                                         observed=sum(r[key] is not None for r in selected),total=len(selected)) for key in ("output_bytes","setup_output_bytes")},
                         timing={key:dict(sum_ms=sum(r["timing"].get(key) for r in selected if r["timing"].get(key) is not None) if any(r["timing"].get(key) is not None for r in selected) else None,
                                          observed=sum(r["timing"].get(key) is not None for r in selected),total=len(selected))
                                 for key in ("wall_ms","setup_ms","provider_ms","graph_sync_ms","plugin_install_ms",
                                             "preflight_outside_wall_ms","preflight_within_setup_ms")})
    repositories = sorted({r["repository"] for r in rows})
    paired_results = paired(rows)
    return dict(schema_version=1, study_id=plan["study_id"], study_kind=kind, source_truth_admission=admission,
                effectiveness_evidence=False, decision="offline-development-only", arms=arms, paired=paired_results,
                repository_sensitivity={repo:paired([r for r in rows if r["repository"] != repo]) for repo in repositories},
                component_sensitivity={component:paired([r for r in rows if r["component"] != component])
                                       for component in sorted({r["component"] for r in rows})},
                families={family:paired([r for r in rows if r["family"] == family]) for family in sorted({r["family"] for r in rows})},
                attempts=rows, telemetry_contract="prospective-accounting-v1; offline fixtures explicitly synthetic",
                timing_boundary="wall includes setup and provider; graph_sync and plugin_install are components, never added again",
                limitations=["Development bootstrap intervals are descriptive; no confirmatory go decision.",
                             "Pending independent review is unknown, never counted as correct or incorrect.",
                             "Token pairs with missing fields stay in all-attempt quality denominators; efficiency is conditional on observed usage.",
                             "No patch-task, chronological-history, cross-language or provider superiority certification."])


def precision(report, protocol):
    if report.get("schema_version") != 1 or not report.get("paired"):
        raise ValueError("development score report required")
    paired_data = report["paired"]
    if paired_data["complete_quality_cases"] != paired_data["total_cases"]:
        raise ValueError("precision planning requires all scheduled cases adjudicated; pending cases cannot be dropped")
    differences = list(paired_data["per_case_quality_delta"].values())
    if len(differences) < 2:
        raise ValueError("at least two independent case clusters required")
    discordance = paired_data["discordance"]
    q = (discordance.get("graph_only",0)+discordance.get("baseline_only",0))/paired_data["total_pairs"]
    alpha = protocol["confirmatory"]["familywise_alpha"] / protocol["confirmatory"]["maximum_tests"]
    z, power_z = NormalDist().inv_cdf(1-alpha/2), NormalDist().inv_cdf(0.8)
    # Plug-in planning is uncertain: show both empirical case variance and the
    # full single-session discordance envelope. Repeated sessions are not n cases.
    v = variance(differences)
    qualified = len(differences) >= 8 and v > 0 and q > 0
    half_widths = {str(n):z*math.sqrt(v/n) if qualified else None for n in (48,96,192,384)}
    approximate_power_n = math.ceil((z+power_z)**2 * max(q-0.1**2,v) / 0.1**2) if qualified else None
    precision_n = math.ceil(z*z*v/0.05**2) if qualified else None
    return dict(schema_version=1, phase="planning-only", target_accuracy_gain=0.1, target_half_width=0.05,
                observed_case_clusters=len(differences), observed_sessions=paired_data["total_pairs"],
                development_discordance=q, development_cluster_variance=v, per_test_alpha=alpha,
                qualification="empirical-planning-diagnostic" if qualified else "insufficient-development-variation",
                minimum_diagnostic_clusters=8, minimum_is_policy_not_power_guarantee=True,
                approximate_80_percent_power_cases=approximate_power_n, approximate_precision_cases=precision_n,
                planned_accuracy_half_width=half_widths, proposed_envelope=protocol["confirmatory"]["planning_envelope"],
                sufficient_48_cases=None, efficiency_power="requires observed paired token variance and joint simulation; not inferred from correctness",
                next_steps=["Independent planner simulates case-clustered repeated outcomes using development discordance and within-case correlation, repository random effects and missingness sensitivity.",
                            "Reestimate under pessimistic discordance/variance bounds; plan accuracy superiority, noninferiority and efficiency jointly with multiplicity.",
                            "Freeze fresh case count/repository allocation/repetitions and fixed final analysis before any holdout output; acquire fresh reserve independently.",
                            "If budget cannot achieve useful precision, declare inconclusive; do not lower the decision threshold."],
                limitation="Normal approximations are planning diagnostics, not established power guarantees or a positive study result.")
