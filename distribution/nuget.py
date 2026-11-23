"""Run the public fixture against an audited licensing package."""

from sdk_tooling.nuget import retained_notice_files

PACKAGE = "NaviApps.SoftwareLicensing"


def check_package(
    nuget,
    dotnet,
    *,
    root,
    package,
    version,
    rid,
    library,
    output,
    env,
    exclude_paths,
    native_aot=False,
):
    notices = retained_notice_files(root / "ThirdParty", "licenses")
    digest = nuget.audit_package(
        package,
        expected_files={"LICENSE": root / "LICENSE", "README.md": root / "README.md", **notices},
        runtime_files={f"runtimes/{rid}/native/{library.name}": library},
    )
    properties = {"Nullable": "enable", "TreatWarningsAsErrors": "true"}
    if native_aot:
        properties.update(
            DefineConstants="NATIVE_AOT_FIXTURE",
            ILLinkTreatWarningsAsErrors="true",
            JsonSerializerIsReflectionEnabledByDefault="false",
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
        properties=properties,
        native_aot=native_aot,
    )
    return digest
