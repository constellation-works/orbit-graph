"""Runnable test-only scoring example. No providers and no historical imports."""
import json
from pathlib import Path
import sys

from source_identity import score_request

ROOT = Path(__file__).resolve().parent / "fixtures"


def selector(language, name, file, line, kind, quote):
    return {"language": language, "name": name, "file": file, "line": line,
            "kind": kind, "citation": {"start_line": line, "end_line": line, "quote": quote}}


def request():
    conversion = selector("rust", "Compass::from_str", "rust/lib.rs", 6, "fn",
                          "    fn from_str(text: &str) -> Result<Self, Self::Err> { todo!() }")
    constant = selector("python", "RETRY_LIMIT", "python/settings.py", 2,
                        "assignment", "RETRY_LIMIT = 7")
    omitted = selector("rust", "Ledger::parse", "rust/lib.rs", 12, "fn",
                       "    pub fn parse(value: u8) -> u8 { value }")
    uncertain = selector("python", "ALIAS", "python/settings.py", 6,
                         "assignment", "ALIAS = RETRY_LIMIT")
    return {"schema_version": 1,
            "manifest": {"rust_root": "rust/lib.rs", "python_files": ["python/settings.py"]},
            "required": [conversion, constant, omitted],
            "submitted": [conversion | {"name": "<Compass as FromStr>::from_str"},
                          constant | {"citation": constant["citation"] | {"quote": "RETRY_LIMIT = 8"}},
                          uncertain]}


if __name__ == "__main__":
    if sys.argv[1:] not in ([], ["--request"]):
        raise SystemExit("usage: python3 -B example.py [--request]")
    output = request() if sys.argv[1:] else score_request(ROOT, request())
    print(json.dumps(output, sort_keys=True, indent=2))
