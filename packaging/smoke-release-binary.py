"""Run the version and MCP initialization checks against the exact release executable."""

import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tomllib


def smoke(command: list[str], version: str) -> None:
    reported = subprocess.run(
        [*command, "--version"], check=True, capture_output=True, text=True, timeout=20
    )
    if reported.stdout.strip() != f"konnect {version}":
        raise ValueError(f"executable reported an unexpected version: {reported.stdout!r}")

    protocol_version = "2025-06-18"
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {"name": "release-binary-smoke", "version": "1"},
        },
    }
    result = subprocess.run(
        command,
        input=json.dumps(request) + "\n",
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        timeout=30,
    )
    reply = json.loads(result.stdout)
    if (
        not isinstance(reply, dict)
        or reply.get("jsonrpc") != "2.0"
        or type(reply.get("id")) is not int
        or reply["id"] != 1
        or "error" in reply
    ):
        raise ValueError(f"unexpected MCP envelope: {reply}")
    payload = reply.get("result", {})
    server_info = payload.get("serverInfo") if isinstance(payload, dict) else None
    if (
        not isinstance(server_info, dict)
        or payload.get("protocolVersion") != protocol_version
        or server_info.get("version") != version
    ):
        raise ValueError(f"unexpected MCP initialize result: {reply}")


def main(target: str) -> None:
    root = Path(__file__).resolve().parent.parent
    with (root / "Cargo.toml").open("rb") as workspace:
        version = tomllib.load(workspace)["workspace"]["package"]["version"]
    suffix = ".exe" if target.endswith("msvc") else ""
    binary = root / "target" / target / "release" / f"konnect{suffix}"
    if sys.platform == "darwin" and target == "x86_64-apple-darwin":
        command = ["arch", "-x86_64", os.fspath(binary)]
    else:
        command = [os.fspath(binary)]
    smoke(command, version)
    print(f"Verified konnect {version} and MCP initialize for {target} (host {platform.machine()})")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: smoke-release-binary.py TARGET")
    try:
        main(sys.argv[1])
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        sys.exit(f"release binary smoke failed: {error}")
