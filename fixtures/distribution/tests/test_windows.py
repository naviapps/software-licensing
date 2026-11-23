"""Exercise the x64 acceptance contract off Windows without starting a worker."""

import hashlib
import importlib.util
import json
import os
import shutil
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from sdk_tooling.adapters import load


def load_driver():
    fixtures = Path(__file__).resolve().parents[1]
    spec = importlib.util.spec_from_file_location(
        "software_licensing_windows", fixtures / "windows.py"
    )
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    return driver


def stage(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    return destination


def publish(stage, destination, verify):
    previous = stage.parent / "previous"
    if destination.exists():
        destination.replace(previous)
    stage.replace(destination)
    try:
        verify()
    except Exception:
        destination.replace(stage)
        if previous.exists():
            previous.replace(destination)
        raise


def artifacts():
    return SimpleNamespace(
        build_lock=nullcontext,
        hashes=lambda files, root: {
            path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in files
        },
        write_json=lambda path, value: path.write_text(json.dumps(value)),
        publish=publish,
    )


class WindowsOutputTests(unittest.TestCase):
    def test_redirected_x64_build_uses_one_native_candidate_for_all_fixtures(self):
        for host, stale, retain in (
            ("arm64", False, False),
            ("arm64", True, False),
            ("arm64", False, True),
            ("x64", False, True),
        ):
            with (
                self.subTest(host=host, stale_legacy_output=stale, retain_package=retain),
                tempfile.TemporaryDirectory() as temporary,
            ):
                self.exercise_driver(Path(temporary), host=host, stale=stale, retain=retain)

    def test_rejects_changed_candidate_before_publishing(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.exercise_driver(Path(temporary), mutate=True)

    def test_rejects_changed_checkout_tools_before_publishing(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.exercise_driver(Path(temporary), change_tools=True)

    def test_rejects_captured_source_for_a_different_actions_revision(self):
        driver = load_driver()
        with patch.object(driver, "load", return_value=load("windows_delivery")):
            with patch.dict(
                os.environ,
                {
                    "VM_RUNNER_SOURCE_COMMIT": "a" * 40,
                    "VM_RUNNER_SOURCE_FINGERPRINT": "b" * 64,
                    "GITHUB_SHA": "c" * 40,
                },
                clear=True,
            ):
                with self.assertRaisesRegex(ValueError, "differs from the Actions revision"):
                    driver.acceptance_source()

    def exercise_driver(
        self, root, *, host="arm64", stale=False, retain=False, mutate=False, change_tools=False
    ):
        driver = load_driver()
        fixture = root / "fixtures/dotnet-consumer/Program.cs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text('System.Console.WriteLine("synthetic");')
        (root / "mise.toml").write_text('[tools]\ndotnet = "10.0.403"\n')
        (root / "native.toml").write_bytes(
            b'[[artifacts]]\nid = "sdk"\nsymbol_prefix = "software_licensing_"\nminimum_macos = "26.0"\n'
        )
        output = root / ".native-build"
        built = output / "accepted"
        built.mkdir(parents=True)
        library = built / "software_licensing.dll"
        library.write_bytes(b"accepted SDK DLL")
        import_library = built / "software_licensing.dll.lib"
        import_library.write_bytes(b"accepted import library")
        receipt_path = built / "native-build.json"
        receipt_path.write_bytes(b"native receipt bytes")
        legacy = root / "target" / driver.TARGET / "release"
        stale_assets = root / "bindings/dotnet/runtimes/win-arm64/native"
        stale_assets.mkdir(parents=True)
        (stale_assets / library.name).write_bytes(b"stale ARM64 package DLL")
        if stale:
            legacy.mkdir(parents=True)
            (legacy / library.name).write_bytes(b"stale x64 build DLL")

        dotnet = root / "dotnet-x64/dotnet.exe"
        source = ("a" * 40, "b" * 64)
        tool_inputs = {"mise.toml": "c" * 64}
        msvc_targets = {"arm64": "aarch64-pc-windows-msvc", "x64": driver.TARGET}
        toolchain = {"backend": "msvc", "host": msvc_targets[host], "target": driver.TARGET}
        receipt = {
            "toolchain": toolchain,
            "outputs": artifacts().hashes([library, import_library], built),
        }
        ci = root / "ci"
        environment = {
            "DOTNET_BIN": str(dotnet),
            "Dotnet_Host_Path": "unexpected ARM64 executable",
            "VM_RUNNER_SOURCE_COMMIT": source[0],
            "VM_RUNNER_SOURCE_FINGERPRINT": source[1],
        }
        if retain:
            environment["VM_RUNNER_ARTIFACTS_DIR"] = str(ci)
        env = dict(environment, PATH="owned-toolchain", CARGO_TARGET_DIR=str(root / "redirected"))
        local_modules = {
            "vm_environment": load("vm_environment"),
            "windows_delivery": load("windows_delivery"),
            "windows": Mock(),
            "bindings": Mock(),
            "native": Mock(),
            "artifacts": artifacts(),
        }
        windows = local_modules["windows"]
        windows.host_architecture.return_value = host
        windows.MSVC_TARGETS = msvc_targets
        bindings = local_modules["bindings"]
        native = local_modules["native"].NativeBuilder.return_value
        native.version = "0.1.0"
        windows.native_environment.return_value = (env, root / "MSVC", dotnet)
        windows.build_library.return_value = built
        windows.verify_artifact.return_value = (built, receipt)
        windows.verify_library.return_value = {"software_licensing_abi_version"}
        bindings.stage_native.side_effect = stage

        package_name = "NaviApps.SoftwareLicensing.0.1.0.nupkg"
        package_bytes = b"accepted NuGet package"
        package_hash = hashlib.sha256(package_bytes).hexdigest()

        def pack(_dotnet, _project, *, output, properties, **_):
            assets = Path(properties["SoftwareLicensingNativeAssets"])
            self.assertEqual(
                [path.relative_to(assets).as_posix() for path in assets.rglob("*.dll")],
                ["win-x64/native/software_licensing.dll"],
            )
            output.mkdir()
            (output / package_name).write_bytes(package_bytes)

        def audit(_package, *, runtime_files, **_):
            self.assertEqual(set(runtime_files), {"runtimes/win-x64/native/software_licensing.dll"})
            (dll,) = runtime_files.values()
            self.assertEqual(dll.read_bytes(), library.read_bytes())
            if mutate:
                dll.write_bytes(b"changed after native fixture")
            return package_hash

        bindings.pack_project.side_effect = pack
        bindings.audit_package.side_effect = audit

        def check_output(command, **_):
            self.assertEqual(command, [str(dotnet), "--version"])
            return "10.0.403\n"

        with (
            patch.object(driver, "ROOT", root),
            patch.object(driver.sys, "platform", "win32"),
            patch.object(
                driver.sys,
                "getwindowsversion",
                return_value=SimpleNamespace(build=26100),
                create=True,
            ),
            patch.dict(os.environ, environment, clear=True),
            patch.object(driver, "load", side_effect=local_modules.__getitem__),
            patch.object(
                driver,
                "support_inputs",
                side_effect=[tool_inputs, {} if change_tools else tool_inputs],
            ),
            patch.object(driver.subprocess, "run") as commands,
            patch.object(driver.subprocess, "check_output", side_effect=check_output),
            patch.object(driver.platform, "platform", return_value="win32-arm64"),
            patch("builtins.print"),
        ):
            if mutate:
                with self.assertRaisesRegex(ValueError, "Staged DLL or import library differs"):
                    driver.main([])
                self.assertFalse((output / driver.RID).exists())
                return
            if change_tools:
                with self.assertRaisesRegex(ValueError, "build tools changed during acceptance"):
                    driver.main([])
                self.assertFalse((output / driver.RID).exists())
                return
            driver.main([])

        expected_environment = dict(environment)
        expected_environment.pop("Dotnet_Host_Path")
        windows.native_environment.assert_called_once_with(
            host, expected_environment, target_architecture="x64"
        )
        windows.build_library.assert_called_once_with(
            native, env, root / "MSVC", "x64", native_host=host
        )
        windows.verify_artifact.assert_called_once_with(native, "x64", environment=env)
        local_modules["native"].NativeBuilder.assert_called_once_with(
            root.resolve(),
            symbol_prefix="software_licensing_",
            minimum_macos="26.0",
            specification="native.toml",
        )
        self.assertTrue(
            all(
                call.args[0][call.args[0].index("--target") + 1] == driver.TARGET
                for call in commands.call_args_list
                if "--target" in call.args[0]
            )
        )
        self.assertFalse(
            any(call.args[0][:2] == ["cargo", "build"] for call in commands.call_args_list)
        )
        c = bindings.compile_c.call_args.kwargs
        candidate = c["output"].parent
        accepted_dll = candidate / library.name
        self.assertEqual(c["libraries"], [candidate / import_library.name])
        self.assertEqual(c["compiler"], [root / "MSVC/cl.exe"])
        windows.require_pe_machine.assert_called_once_with(c["output"], "x64")
        self.assertEqual(windows.verify_library.call_args.args[0], accepted_dll)
        managed = bindings.run_managed.call_args.kwargs
        self.assertEqual(managed["runtime"], "win-x64")
        self.assertEqual(managed["properties"], {"UseAppHost": "false"})
        self.assertEqual(managed["env"]["SOFTWARE_LICENSING_NATIVE_LIBRARY"], str(accepted_dll))
        fixture_call = bindings.run_isolated_consumer.call_args.kwargs
        self.assertEqual(fixture_call["runtime"], "win-x64")
        self.assertEqual(fixture_call["source"], fixture.read_text())
        self.assertIn(built, fixture_call["exclude_paths"])
        self.assertIn(candidate, fixture_call["exclude_paths"])
        self.assertEqual(fixture_call["clear_environment"], ("SOFTWARE_LICENSING_NATIVE_LIBRARY",))

        local = output / driver.RID / native.version
        retained = json.loads((local / driver.RECEIPT).read_bytes())
        self.assertEqual((retained["source"], retained["workspaceFingerprint"]), source)
        execution = "x64-native" if host == "x64" else "x64-emulated-on-arm64"
        self.assertEqual(retained["execution"], execution)
        self.assertEqual(retained["toolchain"], toolchain)
        self.assertEqual(retained["toolInputs"], tool_inputs)
        self.assertEqual(retained["nativeBuildReceiptSha256"], driver.sha256(receipt_path))
        self.assertEqual(retained["dllSha256"], driver.sha256(library))
        self.assertEqual(retained["packages"], {package_name: package_hash})
        self.assertEqual((local / package_name).read_bytes(), package_bytes)
        self.assertEqual((stale_assets / library.name).read_bytes(), b"stale ARM64 package DLL")
        if retain:
            external = ci / driver.RID / native.version
            self.assertEqual(
                (external / driver.RECEIPT).read_bytes(), (local / driver.RECEIPT).read_bytes()
            )
            self.assertEqual((external / package_name).read_bytes(), package_bytes)

    def test_refuses_arm64_package_and_pre_26100_windows(self):
        driver = load_driver()
        with (
            patch("sys.stderr"),
            self.assertRaises(SystemExit),
        ):
            driver.main(["--architecture", "arm64"])
        with (
            patch.object(driver.sys, "platform", "win32"),
            patch.object(
                driver.sys,
                "getwindowsversion",
                return_value=SimpleNamespace(build=26000),
                create=True,
            ),
            patch.object(driver, "load") as load,
            self.assertRaisesRegex(ValueError, "26100 or later"),
        ):
            driver.main([])
        load.assert_not_called()

    def test_external_failure_preserves_previous_pair_in_both_locations(self):
        driver = load_driver()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output, external = root / "local", root / "external"
            feeds = [location / "win-x64/0.1.0" for location in (output, external)]
            package = root / "test.nupkg"
            package.write_bytes(b"new")
            for feed in feeds:
                feed.mkdir(parents=True)
                (feed / package.name).write_bytes(b"previous")
                (feed / driver.RECEIPT).write_bytes(b"previous receipt")
            tooling = artifacts()

            def fail_external(stage, destination, verify):
                if destination == feeds[1]:
                    raise OSError("external publication failed")
                publish(stage, destination, verify)

            tooling.publish = fail_external
            with self.assertRaisesRegex(OSError, "external publication failed"):
                driver.publish_x64(
                    package,
                    {"version": "0.1.0", "packages": {package.name: driver.sha256(package)}},
                    output_dir=output,
                    artifact_root=external,
                    tooling=tooling,
                )
            for feed in feeds:
                self.assertEqual((feed / package.name).read_bytes(), b"previous")
                self.assertEqual((feed / driver.RECEIPT).read_bytes(), b"previous receipt")


if __name__ == "__main__":
    unittest.main()
