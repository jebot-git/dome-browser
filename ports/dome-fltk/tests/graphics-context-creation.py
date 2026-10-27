#!/usr/bin/env python3
"""Validate local EGL context creation without a browser or a GPU reset.

Builds the actual vendored Surfman source with release optimization, executes its
attribute unit tests, then checks native/software contexts and rendered pixels.
Browser loss notification, restoration and adapter migration are separate gates.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
MODES = {"default", "notified", "robust", "robust-notified"}
CHECKS = (
    "actual_strategy_verified", "actual_robust_access_verified",
    "descriptor_roundtrip", "noncurrent_descriptor_verified",
    "borrowed_native_descriptor_verified", "native_import_binding_restored",
    "shared_context_verified", "incompatible_share_rejected", "render_readback_verified",
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source_paths = [ROOT / "Cargo.lock", ROOT / "Cargo.toml", Path(__file__).resolve(),
                    ROOT / "ports/dome-fltk/examples/graphics-reset-probe.rs"]
    source_paths += sorted(p for p in (ROOT / "third_party/surfman").rglob("*") if p.is_file())
    manifest = {str(p.relative_to(ROOT)): sha(p) for p in source_paths}
    (output / "source-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    env = os.environ.copy()
    env.update(CARGO_BUILD_JOBS="1", CARGO_INCREMENTAL="0")
    env.pop("CARGO_TARGET_DIR", None)
    with tempfile.TemporaryDirectory(prefix="dome-graphics-creation-") as temporary:
        harness = Path(temporary)
        vendor = ROOT / "third_party/surfman"
        source = ROOT / "ports/dome-fltk/examples/graphics-reset-probe.rs"
        attributes = vendor / "src/base/egl/robustness.rs"
        (harness / "Cargo.toml").write_text(f'''[package]
name = "dome-graphics-context-creation-validation"
version = "0.1.0"
edition = "2021"
[workspace]
[dependencies]
surfman = {{ path = {json.dumps(str(vendor))} }}
glow = "=0.17.0"
euclid = "0.22"
serde_json = "1"
[[bin]]
name = "graphics-reset-probe"
path = {json.dumps(str(source))}
[[test]]
name = "creation-attributes"
path = "attributes.rs"
''')
        (harness / "attributes.rs").write_text(f'''pub use surfman::{{ContextAttributeFlags, Error, GLApi}};
pub type Gl = glow::Context;
#[allow(dead_code)]
#[path = {json.dumps(str(attributes))}]
mod robustness;
''')
        shutil.copyfile(ROOT / "Cargo.lock", harness / "Cargo.lock")

        def cargo(arguments, log):
            with (output / log).open("w") as stream:
                subprocess.run([args.cargo, *arguments, "--manifest-path", str(harness / "Cargo.toml")],
                               check=True, env=env, stdout=stream, stderr=subprocess.STDOUT)

        # Resolve this small harness against the copied lock; no engine packages
        # are upgraded, and all subsequent compilation is locked and offline.
        cargo(["metadata", "--offline", "--format-version", "1"], "resolution.log")
        shutil.copyfile(harness / "Cargo.lock", output / "harness-Cargo.lock")
        cargo(["test", "--release", "--locked", "--offline", "--test", "creation-attributes"], "unit-tests.log")
        unit_result = re.search(r"test result: ok\. (\d+) passed; 0 failed", (output / "unit-tests.log").read_text())
        assert unit_result and int(unit_result.group(1)) == 7, "attribute unit-test coverage changed"
        cargo(["build", "--release", "--locked", "--offline", "--bin", "graphics-reset-probe"], "build.log")
        binary = output / "graphics-reset-probe"
        shutil.copy2(harness / "target/release/graphics-reset-probe", binary)
        reports = {}
        for mode in ["native", "software"]:
            runtime_env = env.copy()
            runtime_env.pop("LIBGL_ALWAYS_SOFTWARE", None)
            command = [str(binary)]
            if mode == "software":
                runtime_env["LIBGL_ALWAYS_SOFTWARE"] = "1"
                command.append("--software")
            with (output / f"{mode}.jsonl").open("w") as stdout, (output / f"{mode}.stderr").open("w") as stderr:
                subprocess.run(command, check=True, env=runtime_env, stdout=stdout, stderr=stderr)
            rows = [json.loads(line) for line in (output / f"{mode}.jsonl").read_text().splitlines()]
            assert len(rows) == 4 and {row["mode"] for row in rows} == MODES
            for row in rows:
                assert row["software_requested"] == (mode == "software")
                assert row["renderer"] and all(row.get(check) is True for check in CHECKS), row
            reports[mode] = rows
    assert manifest == {str(p.relative_to(ROOT)): sha(p) for p in source_paths}, "source changed during validation"
    result = {
        "finished_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "passed": True, "attribute_unit_tests_passed": 7,
        "renderer_configurations_passed": 8, "assertion_groups_per_configuration": list(CHECKS),
        "binary_sha256": sha(binary), "reports": reports,
        "scope": "Independent Surfman EGL contexts, native import, sharing and pixel readback",
        "browser_recovery_validated": False, "hardware_reset_requested": False,
    }
    (output / "report.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({key: value for key, value in result.items() if key != "reports"}, indent=2))


if __name__ == "__main__":
    main()
