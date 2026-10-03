"""Independent source fixtures; exercise the public API and executable surface."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from source_identity import Project, ContractError, score

HERE = Path(__file__).resolve().parent


def rust(name, line, kind="fn", file="rust/lib.rs", quote=None):
    return {"language": "rust", "name": name, "file": file, "line": line,
            "kind": kind, "citation": {"start_line": line, "end_line": line,
            "quote": quote if quote is not None else
            (HERE / "fixtures" / file).read_text().splitlines()[line - 1]}}


def python(name, line, kind="assignment"):
    return {"language": "python", "name": name, "file": "python/settings.py",
            "line": line, "kind": kind, "citation": {"start_line": line,
            "end_line": line, "quote": (HERE / "fixtures/python/settings.py")
            .read_text().splitlines()[line - 1]}}


class SourceIdentityTests(unittest.TestCase):
    def setUp(self):
        self.project = Project(HERE / "fixtures", rust_root="rust/lib.rs",
                               python_files=["python/settings.py"])

    def test_trait_and_owner_variants(self):
        spellings = ["Compass::from_str", "<Compass as FromStr>::from_str",
                     "<crate::Compass as std::str::FromStr>::from_str"]
        results = [self.project.check(rust(name, 6)) for name in spellings]
        self.assertTrue(all(x["identity_ok"] and x["citation_ok"] for x in results))
        self.assertEqual(len({json.dumps(x["identity"], sort_keys=True) for x in results}), 1)
        self.assertEqual(results[0]["identity"]["trait"], "std::str::FromStr")

    def test_owner_free_nested_and_explicit_module(self):
        for item in [rust("Compass::parse", 9), rust("Ledger::parse", 12),
                     rust("crate::parse", 14), rust("nested::parse", 20),
                     rust("nested::Dial::turn", 18),
                     rust("wire::Socket::connect", 3, file="rust/relocated.rs")]:
            with self.subTest(item=item):
                result = self.project.check(item)
                self.assertTrue(result["identity_ok"], result)
                self.assertTrue(result["citation_ok"], result)
        self.assertFalse(self.project.check(rust("parse", 14))["identity_ok"])

    def test_python_declarations_and_static_assignments(self):
        for item in [python("RETRY_LIMIT", 2), python("python.settings.WINDOW", 3),
                     python("LABELS", 4), python("decode", 8, "function"),
                     python("Packet.decode", 12, "function")]:
            with self.subTest(item=item):
                self.assertTrue(self.project.check(item)["identity_ok"])
        for name, line in [("DYNAMIC", 5), ("ALIAS", 6), ("counterfeit", 15)]:
            self.assertFalse(self.project.check(python(name, line))["identity_ok"])

    def test_adversarial_selectors(self):
        original = rust("Compass::parse", 9)
        changes = [{"name": "Ledger::parse"}, {"name": "wrong::Compass::parse"},
                   {"name": "<Compass as Wrong>::parse"}, {"name": "Compass.parse"},
                   {"file": "rust/relocated.rs"}, {"file": "../lib.rs"},
                   {"kind": "struct"}, {"line": 10}, {"line": True},
                   {"language": "Rust"}, {"name": "counterfeit"},
                   {"name": "Compass::::parse"}, {"ignored": "field"}]
        for change in changes:
            with self.subTest(change=change):
                self.assertFalse(self.project.check(original | change)["identity_ok"])

    def test_citations_are_separate_and_exact(self):
        original = rust("Compass::parse", 9)
        for citation in [{"start_line": 9, "end_line": 9, "quote": "pub fn parse"},
                         {"start_line": 8, "end_line": 8, "quote": "impl Compass {"},
                         {"start_line": 9, "end_line": 90, "quote": ""},
                         {"start_line": True, "end_line": 9, "quote": ""},
                         None]:
            result = self.project.check(original | {"citation": citation})
            self.assertTrue(result["identity_ok"], result)
            self.assertFalse(result["citation_ok"], result)

    def test_score_keeps_denominator_and_extras(self):
        required = [rust("Compass::from_str", 6), python("RETRY_LIMIT", 2)]
        submitted = [rust("<Compass as FromStr>::from_str", 6),
                     rust("<Compass as FromStr>::from_str", 6),
                     python("ALIAS", 6), rust("Ledger::parse", 12)]
        result = score(self.project, required, submitted)
        self.assertEqual(result["identity_recall"], {"matched": 1, "required": 2})
        self.assertEqual(result["citation_recall"], {"matched": 1, "required": 2})
        self.assertEqual(len(result["items"]), 4)
        self.assertEqual(result["semantic_review"]["status"], "pending")
        self.assertEqual(len(result["missing"]), 1)
        self.assertEqual(result["items"][-1]["classification"], "verified_extra")
        self.assertIn("contract_sha256", result["pins"])
        self.assertEqual(len(result["pins"]["source_sha256"]), 3)
        with self.assertRaises(ContractError):
            score(self.project, [python("ALIAS", 6)], [])

    def synthetic(self, files, rust_root=None, python_files=()):
        scratch = HERE.parents[1] / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="source-identity-test-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for name, source in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        return Project(root, rust_root=rust_root, python_files=python_files)

    def test_rust_ambiguous_and_unsupported_contexts(self):
        cases = [
            "fn choose() {}\nfn choose() {}\n",
            '#[cfg(unix)] fn choose() {}\n#[cfg(windows)] fn choose() {}',
            'macro_rules! make { () => { fn choose() {} } } make!();',
            'use crate::hidden::*; fn choose() {}',
            'fn choose() { "unterminated }',
            'fn choose( {}',
            'struct Box; impl Missing { fn choose() {} }',
            'struct Box; impl Unknown for Box { fn choose() {} }',
            'struct Box; impl<T> Box { fn choose() {} }',
            'struct Box; impl Bo x { fn choose() {} }',
            'mod other; fn choose() {}',
        ]
        for source in cases:
            with self.subTest(source=source):
                project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
                item = rust("choose", 1, file="rust/lib.rs") | {"file": "lib.rs"}
                self.assertFalse(project.check(item)["identity_ok"])
                self.assertIsNotNone(project.check(item)["reason"])

    def test_duplicate_trait_impls_cannot_use_line_to_disambiguate(self):
        source = "struct Box;\ntrait Paint { fn coat(); }\nimpl Paint for Box { fn coat() {} }\nimpl Paint for Box { fn coat() {} }\n"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("<Box as Paint>::coat", 3) | {"file": "lib.rs"}
        self.assertIn("ambiguous", project.check(item)["reason"])

    def test_trait_qualification_disambiguates_inherent_and_trait(self):
        source = "struct Box;\ntrait Paint { fn coat(); }\nimpl Paint for Box { fn coat() {} }\nimpl Box { fn coat() {} }\n"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("<Box as Paint>::coat", 3) | {"file": "lib.rs"}
        self.assertTrue(project.check(item)["identity_ok"])
        self.assertFalse(project.check(item | {"name": "Box::coat"})["identity_ok"])
        self.assertFalse(project.check(item | {"name": "<Box as Missing>::coat"})["identity_ok"])

    def test_owner_leaf_must_itself_be_unique(self):
        source = "mod a { pub struct Box; impl Box { fn coat() {} } }\nmod b { pub struct Box; }"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("a::Box::coat", 1) | {"file": "lib.rs"}
        self.assertTrue(project.check(item)["identity_ok"])
        self.assertFalse(project.check(item | {"name": "Box::coat"})["identity_ok"])

    def test_verified_import_alias_and_nested_trait(self):
        source = "mod defs { pub struct Box; pub trait Paint { fn coat(); } }\nuse crate::defs::Box as CrateBox;\nuse crate::defs::Paint as Finish;\nimpl Finish for CrateBox { fn coat() {} }"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("<CrateBox as Finish>::coat", 4) | {"file": "lib.rs"}
        checked = project.check(item)
        self.assertTrue(checked["identity_ok"], checked)
        self.assertEqual(checked["identity"]["owner"], "crate::defs::Box")
        self.assertEqual(checked["identity"]["trait"], "crate::defs::Paint")

    def test_module_paths_are_not_inferred_from_filename(self):
        bad = rust("relocated::Socket::connect", 3, file="rust/relocated.rs")
        self.assertFalse(self.project.check(bad)["identity_ok"])
        source = '#[path = "part.rs"] mod first;\n#[path = "part.rs"] mod second;'
        project = self.synthetic({"lib.rs": source, "part.rs": "fn f() {}"}, rust_root="lib.rs")
        self.assertIn("reused", project.failures["rust"])
        project = self.synthetic({"lib.rs": '#[path = "../outside.rs"] mod other;'}, rust_root="lib.rs")
        self.assertIn("contained", project.failures["rust"])

    def test_python_rebinding_control_flow_and_decorators(self):
        sources = ["VALUE = 1\nVALUE = 2", "VALUE = 1\nVALUE = other", "if flag:\n    VALUE = 1",
                   "VALUE = 1\ndel VALUE", "VALUE = 1\nVALUE += 1", "from other import *\nVALUE = 1",
                   "@wrapper\ndef VALUE(): pass", "class VALUE(Base): pass", "VALUE: int",
                   "VALUE = alias", "VALUE = func()", "VALUE = [1, 2]", "VALUE = 1 + 2",
                   "VALUE = (1, unknown)", "VALUE =", '"VALUE = 1"', "# VALUE = 1"]
        for source in sources:
            with self.subTest(source=source):
                project = self.synthetic({"settings.py": source}, python_files=["settings.py"])
                item = python("VALUE", 1) | {"file": "settings.py"}
                self.assertFalse(project.check(item)["identity_ok"])

    def test_unsupported_python_binding_still_blocks_ambiguous_leaf(self):
        project = self.synthetic({"one.py": "VALUE = 1", "two.py": "VALUE = other"},
                                 python_files=["one.py", "two.py"])
        item = python("VALUE", 1) | {"file": "one.py"}
        self.assertIn("ambiguous", project.check(item)["reason"])
        self.assertTrue(project.check(item | {"name": "one.VALUE"})["identity_ok"])

    def test_python_source_is_never_executed(self):
        project = self.synthetic({"settings.py": 'DYNAMIC = __import__("pathlib").Path("EXECUTED").touch()'},
                                 python_files=["settings.py"])
        self.assertFalse((project.root / "EXECUTED").exists())
        self.assertTrue(project.diagnostics)

    def test_symlink_source_is_refused(self):
        project = self.synthetic({"real.py": "VALUE = 1"})
        (project.root / "link.py").symlink_to("real.py")
        linked = Project(project.root, python_files=["link.py"])
        self.assertIn("symlink", linked.failures["python"])

    def test_frozen_source_snapshot_and_hash(self):
        import hashlib
        source = "VALUE = 1"
        project = self.synthetic({"settings.py": source}, python_files=["settings.py"])
        (project.root / "settings.py").write_text("VALUE = 2")
        item = python("VALUE", 1) | {"file": "settings.py",
            "citation": {"start_line": 1, "end_line": 1, "quote": source}}
        self.assertTrue(project.check(item)["citation_ok"])
        self.assertEqual(project.pins()["source_sha256"]["settings.py"], hashlib.sha256(source.encode()).hexdigest())

    def test_scorer_real_executable_success_and_bad_request(self):
        from example import request
        project = self.synthetic({})
        input_path = project.root / "request.json"
        input_path.write_text(json.dumps(request()))
        command = [sys.executable, "-B", str(HERE / "source_identity.py"),
                   "--root", str(HERE / "fixtures"), "--input", str(input_path)]
        process = subprocess.run(command, capture_output=True, text=True, timeout=20, env={})
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stderr, "")
        output = json.loads(process.stdout)
        self.assertEqual(output["identity_recall"], {"matched": 2, "required": 3})
        self.assertEqual(output["citation_recall"], {"matched": 1, "required": 3})
        for bad_request in ['{"schema_version": 99}', '{',
                            '{"schema_version": 1, "schema_version": 1}',
                            '{"schema_version": NaN}']:
            with self.subTest(request=bad_request):
                input_path.write_text(bad_request)
                process = subprocess.run(command, capture_output=True, text=True, timeout=20, env={})
                self.assertEqual(process.returncode, 2)
                self.assertEqual(process.stdout, "")
                self.assertEqual(json.loads(process.stderr)["code"], "invalid_input")

    def test_shadowed_standard_library_trait_path_is_refused(self):
        source = "mod std { pub mod str { pub trait FromStr { fn from_str(); } } }\nstruct Box;\nimpl std::str::FromStr for Box { fn from_str() {} }"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("<Box as std::str::FromStr>::from_str", 3) | {"file": "lib.rs"}
        self.assertFalse(project.check(item)["identity_ok"])

    def test_python_short_owner_cannot_hide_another_class(self):
        project = self.synthetic({"one.py": "class Packet:\n    def decode(self): pass",
                                  "two.py": "class Packet: pass"}, python_files=["one.py", "two.py"])
        item = python("Packet.decode", 2, "function") | {"file": "one.py"}
        self.assertFalse(project.check(item)["identity_ok"])
        self.assertTrue(project.check(item | {"name": "one.Packet.decode"})["identity_ok"])

    def test_multiline_citation_and_generic_lifetimes(self):
        source = "struct Box;\nimpl Box {\n    fn borrow<'a, T>(value: &'a T) -> &'a T {\n        value\n    }\n}"
        project = self.synthetic({"lib.rs": source}, rust_root="lib.rs")
        item = rust("Box::borrow", 3) | {"file": "lib.rs", "citation": {
            "start_line": 3, "end_line": 5, "quote": "\n".join(source.splitlines()[2:5])}}
        self.assertTrue(project.check(item)["citation_ok"])
        bad = item | {"citation": item["citation"] | {"end_line": 6}}
        self.assertFalse(project.check(bad)["citation_ok"])

    def test_closed_stdout_is_silent_success(self):
        from example import request
        project = self.synthetic({})
        request_path = project.root / "request.json"
        request_path.write_text(json.dumps(request()))
        process = subprocess.Popen([sys.executable, "-B", str(HERE / "source_identity.py"),
                                    "--root", str(HERE / "fixtures"), "--input", str(request_path)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, env={})
        try:
            process.stdout.close()
            process.wait(timeout=20)
            stderr = process.stderr.read()
            self.assertEqual(process.returncode, 0, stderr)
            self.assertEqual(stderr, b"")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            process.stderr.close()

    def test_cli_help_golden(self):
        process = subprocess.run([sys.executable, "-B", str(HERE / "source_identity.py"), "--help"],
                                 capture_output=True, text=True, timeout=20, env={})
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stderr, "")
        golden = HERE / "cli-help.txt"
        if os.environ.get("UPDATE_GOLDENS") == "1":
            golden.write_text(process.stdout)
        self.assertEqual(process.stdout, golden.read_text())

    def test_python_import_bindings_are_visible_without_importing(self):
        source = "from nonexistent import VALUE as OTHER\nimport also_nonexistent\nVALUE: int = 5"
        project = self.synthetic({"settings.py": source}, python_files=["settings.py"])
        item = python("VALUE", 3) | {"file": "settings.py"}
        self.assertTrue(project.check(item)["identity_ok"])
        self.assertFalse(project.check(item | {"name": "OTHER", "line": 1})["identity_ok"])
        self.assertEqual(len(project.diagnostics), 2)

    @unittest.skipIf(sys.version_info < (3, 12), "Python generic def syntax needs Python 3.12+")
    def test_python_generic_function_syntax(self):
        project = self.synthetic({"settings.py": "def identity[T](value: T) -> T:\n    return value"},
                                 python_files=["settings.py"])
        item = python("identity", 1, "function") | {"file": "settings.py"}
        self.assertTrue(project.check(item)["identity_ok"])

    def test_rejected_truth_cannot_shrink_required_denominator(self):
        valid = rust("Compass::parse", 9)
        with self.assertRaises(ContractError):
            score(self.project, [valid, valid], [valid])
        with self.assertRaises(ContractError):
            score(self.project, [], [valid])
        with self.assertRaises(ContractError):
            score(self.project, [valid | {"citation": None}], [valid])

    def test_executable_example_is_deterministic(self):
        process = subprocess.run([sys.executable, "-B", str(HERE / "example.py")],
                                 capture_output=True, text=True, timeout=20, env={})
        self.assertEqual(process.returncode, 0, process.stderr)
        golden = HERE / "example-result.json"
        if os.environ.get("UPDATE_GOLDENS") == "1":
            golden.write_text(process.stdout)
        self.assertEqual(process.stdout, golden.read_text())


if __name__ == "__main__":
    unittest.main()
