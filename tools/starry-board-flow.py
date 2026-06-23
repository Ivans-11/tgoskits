#!/usr/bin/env python3
"""Composable Linux/StarryOS board workflow helper.

The tool is driven by an explicit board case directory.  A case directory owns
board-flow.toml, optional board-*.toml Starry test configs, and references the
real app directory only when asset build/upload commands need it.
"""

from __future__ import annotations

import argparse
import os
import re
import select
import shlex
import signal
import subprocess
import sys
import tarfile
import tempfile
import termios
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

try:
    import tomllib
except ModuleNotFoundError as exc:  # pragma: no cover - Python < 3.11
    raise SystemExit("Python 3.11+ is required for tomllib") from exc


WORKSPACE = Path(__file__).resolve().parents[1]
DEFAULT_BOARD_IP = "192.168.10.2"
DEFAULT_USER = "orangepi"
DEFAULT_SERIAL = "/dev/tty.usbserial-0001"
DEFAULT_BAUD = 1_500_000
DEFAULT_STARRY_PROMPT = "root@starry:/root #"
DEFAULT_FIT = WORKSPACE / "tmp/axbuild/image.fit"


@dataclass
class Board:
    host: str
    user: str
    password: str | None
    connect_timeout: int


@dataclass
class BoardCase:
    name: str
    case_dir: Path
    app_dir: Path
    flow_path: Path | None
    flow: dict[str, Any]
    board_config_path: Path | None


def log(message: str) -> None:
    print(f"[starry-board-flow] {message}", flush=True)


def resolve_workspace_path(path: str | Path, base: Path = WORKSPACE) -> Path:
    path = Path(path)
    if path.is_absolute():
        return path
    return (base / path).resolve()


def read_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as file:
        return tomllib.load(file)


def load_case(case_dir: str | Path, board_config: str | None = None) -> BoardCase:
    case_dir = resolve_workspace_path(case_dir)
    if not case_dir.is_dir():
        raise SystemExit(f"missing board case directory: {case_dir}")

    flow_path = case_dir / "board-flow.toml"
    if not flow_path.is_file():
        raise SystemExit(f"missing board case flow: {flow_path}")
    flow = read_toml(flow_path)

    case_meta = flow.get("case", {})
    name = case_meta.get("name") or case_dir.name
    app_dir = resolve_workspace_path(case_meta.get("app_dir", "."), case_dir)
    if not app_dir.is_dir():
        raise SystemExit(f"{name}: missing app_dir: {app_dir}")

    if board_config:
        board_config_path = resolve_workspace_path(board_config, case_dir)
    elif "starry" in flow and flow["starry"].get("board_config"):
        board_config_path = resolve_workspace_path(flow["starry"]["board_config"], case_dir)
    else:
        configs = sorted(case_dir.glob("board-*.toml"))
        board_config_path = configs[0] if len(configs) == 1 else None

    return BoardCase(
        name=name,
        case_dir=case_dir,
        app_dir=app_dir,
        flow_path=flow_path if flow_path.is_file() else None,
        flow=flow,
        board_config_path=board_config_path,
    )


def run_local(command: str, cwd: Path) -> None:
    log(f"local: {command}")
    subprocess.run(command, cwd=cwd, shell=True, check=True)


def run_command_capture(
    argv: list[str],
    timeout: int,
    input_data: bytes | None = None,
    env: dict[str, str] | None = None,
) -> str:
    proc = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE if input_data is not None else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
    )
    output = bytearray()
    deadline = time.monotonic() + timeout
    assert proc.stdout is not None
    fd = proc.stdout.fileno()
    if input_data is not None:
        assert proc.stdin is not None
        try:
            proc.stdin.write(input_data)
            proc.stdin.close()
        except BrokenPipeError:
            pass

    def drain_ready() -> None:
        while True:
            ready, _, _ = select.select([fd], [], [], 0)
            if not ready:
                return
            data = os.read(fd, 8192)
            if not data:
                return
            output.extend(data)
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()

    try:
        while True:
            if time.monotonic() > deadline:
                proc.terminate()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill()
                raise TimeoutError(f"command timed out: {shlex.join(argv)}")

            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                data = os.read(fd, 8192)
                if data:
                    output.extend(data)
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                    continue

            rc = proc.poll()
            if rc is not None:
                drain_ready()
                text = bytes(output).decode(errors="ignore")
                if rc != 0:
                    raise subprocess.CalledProcessError(rc, argv, text)
                return text
            if ready:
                time.sleep(0.05)
    finally:
        if proc.poll() is None:
            proc.terminate()


def ssh_argv(board: Board, remote_command: str) -> list[str]:
    argv = [
        "ssh",
        "-T",
        "-o",
        "BatchMode=no",
        "-o",
        "PreferredAuthentications=password,keyboard-interactive,publickey",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/tmp/tgoskits_known_hosts",
        "-o",
        f"ConnectTimeout={board.connect_timeout}",
        f"{board.user}@{board.host}",
        remote_command,
    ]
    if board.password:
        argv = ["sshpass", "-e"] + argv
    return argv


def ssh(
    board: Board,
    remote_command: str,
    timeout: int = 120,
    input_data: bytes | None = None,
    display_command: str | None = None,
) -> str:
    log(f"ssh {board.user}@{board.host}: {display_command or remote_command}")
    env = os.environ.copy()
    if board.password:
        env["SSHPASS"] = board.password
    return run_command_capture(ssh_argv(board, remote_command), timeout, input_data, env)


def scp_to(board: Board, local: Path, remote: str, timeout: int = 300) -> None:
    argv = [
        "scp",
        "-o",
        "BatchMode=no",
        "-o",
        "PreferredAuthentications=password,keyboard-interactive,publickey",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/tmp/tgoskits_known_hosts",
        "-o",
        f"ConnectTimeout={board.connect_timeout}",
        str(local),
        f"{board.user}@{board.host}:{remote}",
    ]
    if board.password:
        argv = ["sshpass", "-e"] + argv
    log(f"scp {local} -> {board.user}@{board.host}:{remote}")
    env = os.environ.copy()
    if board.password:
        env["SSHPASS"] = board.password
    run_command_capture(argv, timeout, env=env)


def sudo_script(board: Board, script: str, timeout: int = 120) -> str:
    quoted = shlex.quote(script)
    remote = f"sudo -S sh -c {quoted}"
    input_data = f"{board.password}\n".encode() if board.password else None
    return ssh(board, remote, timeout, input_data=input_data)


def make_tar_from_dir(source: Path) -> Path:
    if not source.exists():
        raise SystemExit(f"missing asset source: {source}")
    fd, tar_name = tempfile.mkstemp(prefix="starry-board-assets.", suffix=".tar.gz")
    os.close(fd)
    tar_path = Path(tar_name)
    with tarfile.open(tar_path, "w:gz") as tar:
        if source.is_dir():
            for child in sorted(source.rglob("*")):
                tar.add(child, arcname=child.relative_to(source))
        else:
            tar.add(source, arcname=source.name)
    return tar_path


def upload_directory(board: Board, source: Path, target: str, timeout: int = 600) -> None:
    tar_path = make_tar_from_dir(source)
    remote_tar = f"/tmp/{tar_path.name}"
    try:
        scp_to(board, tar_path, remote_tar, timeout)
        script = f"""
set -e
rm -rf {shlex.quote(target)}
mkdir -p {shlex.quote(target)}
tar -xzf {shlex.quote(remote_tar)} -C {shlex.quote(target)}
chown -R root:root {shlex.quote(target)}
rm -f {shlex.quote(remote_tar)}
sync
ls -ld {shlex.quote(target)}
"""
        sudo_script(board, script, timeout)
    finally:
        tar_path.unlink(missing_ok=True)


def upload_file(board: Board, source: Path, target: str, timeout: int = 300) -> None:
    if not source.is_file():
        raise SystemExit(f"not a regular file: {source}")
    remote_tmp = f"/tmp/{source.name}.{int(time.time())}"
    scp_to(board, source, remote_tmp, timeout)
    script = f"""
set -e
mkdir -p "$(dirname {shlex.quote(target)})"
cp {shlex.quote(remote_tmp)} {shlex.quote(target)}
chown root:root {shlex.quote(target)}
rm -f {shlex.quote(remote_tmp)}
sync
ls -l {shlex.quote(target)}
"""
    sudo_script(board, script, timeout)


def build_assets(case: BoardCase) -> None:
    assets = case.flow.get("assets", {})
    commands = assets.get("build", [])
    if isinstance(commands, str):
        commands = [commands]
    if not commands:
        log(f"{case.name}: no [assets].build commands")
        return
    for command in commands:
        run_local(command, case.app_dir)


def upload_configured_assets(board: Board, case: BoardCase, timeout: int) -> None:
    assets = case.flow.get("assets", {})
    items = assets.get("items", [])
    if not items:
        raise SystemExit(f"{case.name} has no [[assets.items]] in board-flow.toml")
    for item in items:
        source = resolve_workspace_path(item["source"], case.app_dir)
        target = item["target"]
        log(f"upload asset {source} -> {target}")
        if source.is_dir():
            upload_directory(board, source, target, timeout)
        else:
            upload_file(board, source, target, timeout)


def upload_fit(board: Board, fit: Path, timeout: int) -> None:
    if not fit.is_file():
        raise SystemExit(f"FIT image does not exist: {fit}")
    remote_tmp = f"/tmp/image.fit.{int(time.time())}"
    scp_to(board, fit, remote_tmp, timeout)
    script = f"""
set -e
mkdir -p /boot/starry
cp {shlex.quote(remote_tmp)} /boot/starry/image.fit
rm -f {shlex.quote(remote_tmp)}
sync
sha256sum /boot/starry/image.fit
"""
    sudo_script(board, script, timeout)


def regex_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    return list(value)


def assert_output(
    output: str,
    success_regex: list[str],
    fail_regex: list[str],
    label: str,
) -> None:
    for pattern in fail_regex:
        if re.search(pattern, output):
            raise SystemExit(f"{label}: fail regex matched: {pattern}")
    for pattern in success_regex:
        if not re.search(pattern, output):
            raise SystemExit(f"{label}: success regex did not match: {pattern}")


def linux_test(board: Board, case: BoardCase, timeout: int, command: str | None = None) -> None:
    linux = case.flow.get("linux", {})
    command = command or linux.get("command")
    if not command:
        raise SystemExit(f"{case.name} has no [linux].command; pass --command")
    remote_command = command
    input_data = None
    if board.password:
        remote_command = f"IFS= read -r BOARD_PASSWORD; export BOARD_PASSWORD; sh -c {shlex.quote(command)}"
        input_data = f"{board.password}\n".encode()
    output = ssh(
        board,
        remote_command,
        timeout or int(linux.get("timeout", 120)),
        input_data=input_data,
        display_command=command,
    )
    assert_output(
        output,
        regex_list(linux.get("success_regex")),
        regex_list(linux.get("fail_regex")),
        "linux-test",
    )


def parse_board_test_config(path: Path) -> dict[str, Any]:
    if not path or not path.is_file():
        raise SystemExit(f"missing board config: {path}")
    return read_toml(path)


def starry_command_from_config(case: BoardCase, command: str | None) -> tuple[str, list[str], list[str], int, str]:
    if command:
        return command, [], [], 180, DEFAULT_STARRY_PROMPT
    if not case.board_config_path:
        raise SystemExit(f"{case.name} has multiple/no board configs; pass --board-config or --command")
    config = parse_board_test_config(case.board_config_path)
    command = config.get("shell_init_cmd")
    if not command:
        init_path = case.app_dir / "init.sh"
        command = init_path.read_text().strip() if init_path.is_file() else None
    if not command:
        raise SystemExit(f"{case.name} has no shell_init_cmd or init.sh")
    return (
        command,
        regex_list(config.get("success_regex")),
        regex_list(config.get("fail_regex")),
        int(config.get("timeout", 180)),
        config.get("shell_prefix", DEFAULT_STARRY_PROMPT),
    )


def serial_baud_constant(baud: int) -> int:
    name = f"B{baud}"
    if not hasattr(termios, name):
        raise SystemExit(f"unsupported baud rate in termios: {baud}")
    return getattr(termios, name)


def open_serial(port: str, baud: int) -> int:
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    attrs = termios.tcgetattr(fd)
    attrs[0] = 0
    attrs[1] = 0
    attrs[2] &= ~termios.CSIZE
    attrs[2] |= termios.CS8 | termios.CREAD | termios.CLOCAL
    attrs[3] = 0
    speed = serial_baud_constant(baud)
    attrs[4] = speed
    attrs[5] = speed
    attrs[6][termios.VMIN] = 0
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attrs)
    return fd


def serial_write(fd: int, data: str | bytes) -> None:
    if isinstance(data, str):
        data = data.encode()
    os.write(fd, data)


def serial_run(
    port: str,
    baud: int,
    wait_for: str,
    command: str,
    success_regex: list[str],
    fail_regex: list[str],
    timeout: int,
    prompt_nudge_delay: float | None,
) -> str:
    fd = open_serial(port, baud)
    buf = bytearray()
    injected = False
    deadline = time.monotonic() + timeout
    last_activity = time.monotonic()
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                data = os.read(fd, 8192)
                if data:
                    last_activity = time.monotonic()
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                    buf.extend(data)
                    text = bytes(buf).decode(errors="ignore")
                    match_text = text.replace("\r\n", "\n").replace("\r", "\n")

                    if not injected and wait_for in match_text:
                        injected = True
                        serial_write(fd, "\n")
                        time.sleep(0.2)
                        serial_write(fd, command.rstrip() + "\n")

                    if injected:
                        for pattern in fail_regex:
                            if re.search(pattern, match_text):
                                raise SystemExit(f"serial-run: fail regex matched: {pattern}")
                        if success_regex and all(re.search(pattern, match_text) for pattern in success_regex):
                            return text
            if injected and not success_regex:
                return bytes(buf).decode(errors="ignore")
            if (
                not injected
                and prompt_nudge_delay is not None
                and prompt_nudge_delay > 0
                and time.monotonic() - last_activity >= prompt_nudge_delay
            ):
                serial_write(fd, "\n")
                last_activity = time.monotonic()
        raise TimeoutError("serial-run timed out")
    finally:
        os.close(fd)


def request_starry_boot(board: Board, timeout: int) -> None:
    script = """
set -e
cp /boot/starry-boot-1.env /boot/starry-boot.env
sync
cat /boot/starry-boot.env
reboot
"""
    try:
        sudo_script(board, script, timeout)
    except subprocess.CalledProcessError:
        # Reboot usually closes SSH before it can return cleanly.
        pass


def wait_ssh(board: Board, timeout: int) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            output = ssh(board, "hostname; cat /boot/starry-boot.env 2>/dev/null || true", 15)
            if "orangepi5plus" in output:
                return
        except Exception:
            time.sleep(2)
    raise TimeoutError("Linux SSH did not become ready")


def run_power_cycle(command: str | None) -> None:
    if command:
        run_local(command, WORKSPACE)
        return
    print(
        "\nStarry test finished. Power-cycle or reset the board now; "
        "the one-shot marker has already been cleared by U-Boot.",
        flush=True,
    )


def make_board(args: argparse.Namespace) -> Board:
    password = args.ssh_password
    if password == "":
        password = None
    return Board(args.board_ip, args.ssh_user, password, args.connect_timeout)


def add_common_board_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--board-ip", default=os.getenv("BOARD_IP", DEFAULT_BOARD_IP))
    parser.add_argument("--ssh-user", default=os.getenv("BOARD_USER", DEFAULT_USER))
    parser.add_argument("--ssh-password", default=os.getenv("BOARD_PASSWORD"))
    parser.add_argument("--connect-timeout", type=int, default=8)


def add_common_case_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("case_dir", help="board case directory containing board-flow.toml")
    parser.add_argument("--board-config")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("build-assets")
    add_common_case_args(p)

    p = sub.add_parser("upload-assets")
    add_common_board_args(p)
    add_common_case_args(p)
    p.add_argument("--build", action="store_true", help="run [assets].build before upload")
    p.add_argument("--timeout", type=int, default=600)

    p = sub.add_parser("upload-path")
    add_common_board_args(p)
    p.add_argument("source")
    p.add_argument("target")
    p.add_argument("--timeout", type=int, default=600)

    p = sub.add_parser("upload-fit")
    add_common_board_args(p)
    p.add_argument("--fit", default=str(DEFAULT_FIT))
    p.add_argument("--timeout", type=int, default=600)

    p = sub.add_parser("linux-test")
    add_common_board_args(p)
    add_common_case_args(p)
    p.add_argument("--command")
    p.add_argument("--timeout", type=int, default=0)

    p = sub.add_parser("starry-test")
    add_common_board_args(p)
    add_common_case_args(p)
    p.add_argument("--serial", default=DEFAULT_SERIAL)
    p.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    p.add_argument("--command")
    p.add_argument("--no-reboot", action="store_true")
    p.add_argument("--timeout", type=int, default=0)
    p.add_argument("--prompt-nudge-delay", type=float, default=10.0)
    p.add_argument("--no-prompt-nudge", action="store_true")

    p = sub.add_parser("serial-run")
    p.add_argument("--serial", default=DEFAULT_SERIAL)
    p.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    p.add_argument("--wait-for", default=DEFAULT_STARRY_PROMPT)
    p.add_argument("--command", required=True)
    p.add_argument("--success-regex", action="append", default=[])
    p.add_argument("--fail-regex", action="append", default=[])
    p.add_argument("--timeout", type=int, default=180)
    p.add_argument("--prompt-nudge-delay", type=float, default=1.0)
    p.add_argument("--no-prompt-nudge", action="store_true")

    p = sub.add_parser("run")
    add_common_board_args(p)
    add_common_case_args(p)
    p.add_argument("--serial", default=DEFAULT_SERIAL)
    p.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    p.add_argument("--deploy-assets", action="store_true")
    p.add_argument("--build-assets", action="store_true")
    p.add_argument("--deploy-fit", action="store_true")
    p.add_argument("--fit", default=str(DEFAULT_FIT))
    p.add_argument("--side", choices=["both", "linux", "starry"], default="both")
    p.add_argument("--skip-linux", action="store_true")
    p.add_argument("--skip-starry", action="store_true")
    p.add_argument("--linux-command")
    p.add_argument("--starry-command")
    p.add_argument("--power-cycle-command")
    p.add_argument("--wait-linux", action="store_true")
    p.add_argument("--timeout", type=int, default=600)
    p.add_argument("--prompt-nudge-delay", type=float, default=10.0)
    p.add_argument("--no-prompt-nudge", action="store_true")

    args = parser.parse_args()

    if args.cmd == "upload-path":
        board = make_board(args)
        source = resolve_workspace_path(args.source)
        if source.is_dir():
            upload_directory(board, source, args.target, args.timeout)
        else:
            upload_file(board, source, args.target, args.timeout)
        return 0

    if args.cmd == "upload-fit":
        upload_fit(make_board(args), resolve_workspace_path(args.fit), args.timeout)
        return 0

    if args.cmd == "serial-run":
        serial_run(
            args.serial,
            args.baud,
            args.wait_for,
            args.command,
            args.success_regex,
            args.fail_regex,
            args.timeout,
            None if args.no_prompt_nudge else args.prompt_nudge_delay,
        )
        return 0

    case = load_case(args.case_dir, getattr(args, "board_config", None))

    if args.cmd == "build-assets":
        build_assets(case)
        return 0

    if args.cmd == "upload-assets":
        if args.build:
            build_assets(case)
        upload_configured_assets(make_board(args), case, args.timeout)
        return 0

    if args.cmd == "linux-test":
        linux_test(make_board(args), case, args.timeout, args.command)
        return 0

    if args.cmd == "starry-test":
        board = make_board(args)
        command, success, fail, config_timeout, prompt = starry_command_from_config(case, args.command)
        if not args.no_reboot:
            request_starry_boot(board, args.timeout or 60)
        serial_run(
            args.serial,
            args.baud,
            prompt,
            command,
            success,
            fail,
            args.timeout or config_timeout,
            None if args.no_prompt_nudge else args.prompt_nudge_delay,
        )
        return 0

    if args.cmd == "run":
        board = make_board(args)
        if args.deploy_assets:
            if args.build_assets:
                build_assets(case)
            upload_configured_assets(board, case, args.timeout)
        if args.deploy_fit:
            upload_fit(board, resolve_workspace_path(args.fit), args.timeout)
        run_linux = args.side in ("both", "linux") and not args.skip_linux
        run_starry = args.side in ("both", "starry") and not args.skip_starry
        if run_linux:
            linux_test(board, case, args.timeout, args.linux_command)
        if run_starry:
            command, success, fail, config_timeout, prompt = starry_command_from_config(case, args.starry_command)
            request_starry_boot(board, 60)
            serial_run(
                args.serial,
                args.baud,
                prompt,
                command,
                success,
                fail,
                config_timeout,
                None if args.no_prompt_nudge else args.prompt_nudge_delay,
            )
            if args.wait_linux:
                run_power_cycle(args.power_cycle_command)
                wait_ssh(board, args.timeout)
        return 0

    raise AssertionError(args.cmd)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        os.kill(os.getpid(), signal.SIGINT)
