"""Run the public fixture against an audited licensing package."""

PACKAGE = "NaviApps.SoftwareLicensing"


def check_package(
    nuget, dotnet, *, root, package, version, rid, library, output, env, exclude_paths
):
    notices = {
        "licenses/" + path.relative_to(root / "ThirdParty").as_posix(): path
        for path in (root / "ThirdParty").rglob("*")
        if path.is_file() and path.name not in (".DS_Store", "registry-notices.json")
    }
    digest = nuget.audit_package(
        package,
        expected_files={"LICENSE": root / "LICENSE", "README.md": root / "README.md", **notices},
        runtime_files={f"runtimes/{rid}/native/{library.name}": library},
    )
    nuget.run_isolated_consumer(
        dotnet,
        packages=[package],
        references={PACKAGE: version},
        source=(root / "fixtures/consumers/dotnet/Program.cs").read_text(encoding="utf-8"),
        work_parent=output,
        env=env,
        exclude_paths=exclude_paths,
        runtime=rid,
        properties={"Nullable": "enable", "TreatWarningsAsErrors": "true"},
    )
    return digest
