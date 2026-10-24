"""Package release builds and validate the complete set before publishing."""

import argparse
import hashlib
from pathlib import Path
import shutil
import tarfile
import zipfile


RELAY_TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")
AGENT_TARGETS = (
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    *RELAY_TARGETS,
)


def asset_name(component, target=None):
    if component == "deploy" and target is None:
        return "codeg-relay-deploy.tar.gz"
    if component == "relay" and target in RELAY_TARGETS:
        return f"codeg-relay-{target}"
    if component == "agent" and target in AGENT_TARGETS:
        extension = "zip" if target.endswith("windows-msvc") else "tar.gz"
        return f"codeg-relay-agent-{target}.{extension}"
    raise ValueError(f"Unsupported component/target: {component}/{target}")


def checksum_line(path):
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    return f"{digest}  {path.name}\n"


def require_files(paths):
    for path in paths:
        if not path.is_file():
            raise ValueError(f"Missing file: {path}")


def tar_permissions(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mode = 0o755 if info.isdir() or info.mode & 0o111 else 0o644
    return info


def package(args):
    name = asset_name(args.component, args.target)
    notices = [args.repo_root / "LICENSE", args.repo_root / "NOTICE"]
    require_files(notices)
    if args.component == "deploy":
        deploy = args.repo_root / "crates/codeg-relay/deploy"
        require_files([deploy / name for name in (
            "install.sh", "uninstall.sh", "codeg-relay.service",
        )])
        sources = [deploy, *notices]
    else:
        if args.target_dir is None:
            raise ValueError("--target-dir is required for binaries")
        release = args.target_dir / args.target / "release"
        if args.target.endswith("windows-msvc"):
            binaries = sorted(path for path in release.glob("*.exe") if path.is_file())
            if not binaries:
                raise ValueError(f"No .exe files in {release}")
        else:
            binary = "codeg-relay" if args.component == "relay" else "codeg-relay-agent"
            binaries = [release / binary]
        require_files(binaries)
        sources = [*binaries, *notices]

    args.output_dir.mkdir(parents=True, exist_ok=True)
    destination = args.output_dir / name
    if args.component == "relay":
        shutil.copyfile(binaries[0], destination)
        destination.chmod(0o755)
    elif name.endswith(".zip"):
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for source in sources:
                archive.write(source, arcname=source.name)
    else:
        with tarfile.open(destination, "w:gz") as archive:
            for source in sources:
                if args.component == "agent" and source in binaries:
                    info = tar_permissions(archive.gettarinfo(source, arcname=source.name))
                    info.mode = 0o755
                    with source.open("rb") as stream:
                        archive.addfile(info, stream)
                else:
                    archive.add(source, arcname=source.name, filter=tar_permissions)
    (args.output_dir / f"{name}.sha256").write_bytes(checksum_line(destination).encode("ascii"))
    print(name)


def checksums(output_dir):
    assets = {
        asset_name("deploy"),
        *(asset_name("relay", target) for target in RELAY_TARGETS),
        *(asset_name("agent", target) for target in AGENT_TARGETS),
    }
    expected = assets | {f"{name}.sha256" for name in assets}
    actual = {path.name for path in output_dir.iterdir()} - {"SHA256SUMS"}
    if missing := expected - actual:
        raise ValueError(f"Missing release assets: {', '.join(sorted(missing))}")
    if extra := actual - expected:
        raise ValueError(f"Unexpected release assets: {', '.join(sorted(extra))}")
    for name in sorted(assets):
        sidecar = output_dir / f"{name}.sha256"
        if sidecar.read_bytes() != checksum_line(output_dir / name).encode("ascii"):
            raise ValueError(f"Checksum mismatch: {name}")
    # Cover the sidecars as well as the eight payloads; a manifest cannot hash itself.
    manifest = "".join(checksum_line(output_dir / name) for name in sorted(expected))
    (output_dir / "SHA256SUMS").write_bytes(manifest.encode("ascii"))
    print(f"Verified {len(assets)} payloads; SHA256SUMS covers {len(expected)} files")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pack = commands.add_parser("package")
    pack.add_argument("component", choices=("relay", "agent", "deploy"))
    pack.add_argument("--target")
    pack.add_argument("--target-dir", type=Path)
    pack.add_argument("--repo-root", type=Path, default=Path("."))
    pack.add_argument("--output-dir", type=Path, required=True)
    sums = commands.add_parser("checksums")
    sums.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "package":
            package(args)
        else:
            checksums(args.output_dir)
    except (OSError, ValueError) as error:
        parser.exit(1, f"Error: {error}\n")


if __name__ == "__main__":
    main()
