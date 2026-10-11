#!/usr/bin/env python3
"""Analyze this app's RenderService timestamps and anonymous performance counters.

Offline analysis never invokes HDC. Live capture is opt-in, requires an explicit
device and app PID, and touches only feihong-surfaceSurface FPS diagnostics. It
does not install/start the app, change settings, or enable tracing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import unittest


SURFACE = "feihong-surfaceSurface"
SCHEMA_VERSION = 2
ROSEN_FPS_SOURCE = (
    "https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/"
    "render_service_base/src/gfx/fps_info/rs_surface_fps.cpp"
)
MAX_INPUT_BYTES = 2 * 1024 * 1024
SLOT = re.compile(r"\s*(-?\d{1,20}):(-?\d{1,20})\s*")
NUMBER = r"(?:\d{1,20}(?:\.\d{1,9})?|none|true|false)"
LOG_HEADER = re.compile(
    r"^\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d+\s+"
    r"(?P<pid>\d+)\s+\d+\s+[DIWEF]\s+"
    r"(?:[A-Za-z0-9]+/)?(?:moe\.kiwi\.FlectarMail/)?"
    r"(?P<tag>FeihongPerf|Feihong):\s*(?P<body>.*)$"
)
NATIVE_KEYS = {
    "attempts", "presented", "wallMs", "windowFps", "avgMs", "maxMs",
    "requestMs", "fenceMs", "copyMs", "mapCopyMs", "flushMs",
}
NATIVE_DETAIL_KEYS = {"configMs", "mapMs", "pixelCopyMs", "unmapMs"}
NATIVE_CPU_KEYS = {"pixelCopyCpuSamples", "pixelCopyCpuMs", "strideBytes", "bufferUsage"}
RUST_KEYS = {
    "wall_ms", "active_ms", "idle", "attempts", "success", "failed",
    "active_success", "wall_fps", "active_fps", "batches", "events",
}
GROUPS = {"event", "timers", "render", "present", "frame_work", "interval"}
DISTRIBUTION_KEYS = {"samples", "retained", "total_ms", "p50_ms", "p95_ms", "max_ms"}
UI_QOS = re.compile(
    r"UI QoS:\s+interactive=(?P<interactive>[01])\s+"
    r"setStatus=(?P<setStatus>-?\d{1,10})\s+"
    r"beforeStatus=(?P<beforeStatus>-?\d{1,10})\s+"
    r"beforeLevel=(?P<beforeLevel>-?\d{1,10})\s+"
    r"afterStatus=(?P<afterStatus>-?\d{1,10})\s+"
    r"afterLevel=(?P<afterLevel>-?\d{1,10})"
)
NATIVE_INPUT_BOUNDARY = re.compile(
    r"Native input boundary action=(?P<action>[01]) "
    r"monotonicNs=(?P<monotonic_ns>\d{1,20})"
)
FONT_INIT_MS = r"\d{1,20}(?:\.\d{1,9})?"
FONT_INIT_BACKGROUND = re.compile(
    r"FeihongFontInit background=1 success=(?P<success>[01]) "
    r"published=(?P<published>[01]) bootstrap_ms=(?P<bootstrap_ms>" + FONT_INIT_MS + r")"
)
FONT_INIT_OWNER = re.compile(
    r"FeihongFontInit background=0 ready=(?P<ready>[01]) "
    r"bootstrap_ms=(?P<bootstrap_ms>" + FONT_INIT_MS + r")"
)


class DiagnosticError(Exception):
    """A fixed, sanitized diagnostic error, never a subprocess output."""


def interval_summary(intervals_ns: list[int]) -> dict:
    if not intervals_ns:
        return {"samples": 0, "median": None, "p95": None, "max": None}
    ordered = sorted(intervals_ns)
    return {
        "samples": len(ordered),
        "median": statistics.median(ordered) / 1_000_000,
        "p95": ordered[math.ceil(len(ordered) * 0.95) - 1] / 1_000_000,
        "max": ordered[-1] / 1_000_000,
    }


def frame_pacing_summary(intervals_ns: list[int]) -> dict:
    """Descriptive long-frame counts; never infer animation or remove pauses.

    An isolated animation needs its recorded action context. These thresholds
    describe measured gaps, not a promise that all application UI is smooth.
    """
    result = {"samples": len(intervals_ns), "activity": "not_inferred"}
    for label, limit_ns in [("over_33_33_ms", 33_333_333),
                            ("over_50_ms", 50_000_000),
                            ("over_100_ms", 100_000_000)]:
        count = sum(interval > limit_ns for interval in intervals_ns)
        consecutive = longest = 0
        for interval in intervals_ns:
            consecutive = consecutive + 1 if interval > limit_ns else 0
            longest = max(longest, consecutive)
        result[label] = {
            "count": count,
            "ratio": count / len(intervals_ns) if intervals_ns else None,
            "longest_run": longest,
        }
    return result


def fps_pairs(text: str) -> list[tuple[int, int]]:
    """Retain every numeric ring slot in source order, including empty slots."""
    return [tuple(map(int, match.groups())) for line in text.splitlines()
            if (match := SLOT.fullmatch(line))]


def valid_fps_pair(pair: tuple[int, int]) -> bool:
    return 0 < pair[0] <= pair[1] <= 2**64 - 1


def timestamp_column_summary(
    pairs: list[tuple[int, int]], column: int, *, marker_present: bool,
    ring_pairs: list[tuple[int, int]],
) -> dict:
    """Compute one column without inferring activity or dropping any valid gap.

    Ring metadata always describes the original capture, even for candidates.
    Duplicate timestamps invalidate the rate rather than count the same moment
    twice. Full valid timestamp and gap lists remain available for audit.
    """
    zero_slots = sum(pair == (0, 0) for pair in ring_pairs)
    nonempty = [pair for pair in pairs if pair != (0, 0)]
    invalid = sum(not valid_fps_pair(pair) for pair in nonempty)
    timestamps = [pair[column] for pair in nonempty if valid_fps_pair(pair)]
    duplicates = len(timestamps) - len(set(timestamps))
    source_order_monotonic = all(right > left for left, right in zip(timestamps, timestamps[1:]))
    timestamps.sort()
    intervals = [right - left for left, right in zip(timestamps, timestamps[1:])]
    rate_field = "actual_present_fps" if column == 1 else "observed_flush_fps"
    result = {
        "status": "unavailable",
        "reason": None,
        "surface": SURFACE,
        "timestamp_unit": "ns",
        "timestamp_column": "second_present" if column == 1 else "first_flush",
        "frame_count": len(timestamps),
        "ring_slot_count": len(ring_pairs),
        "zero_slots": zero_slots,
        "ring_buffer_truncated_potential": bool(ring_pairs) and zero_slots == 0,
        "invalid_slots": invalid,
        "duplicate_timestamps": duplicates,
        "source_order_monotonic": source_order_monotonic,
        "span_seconds": None,
        rate_field: None,
        "all_sorted_timestamps_ns": timestamps,
        "all_intervals_ms": [interval / 1_000_000 for interval in intervals],
        "intervals_ms": interval_summary([]),
        "frame_pacing": None,
        "activity": "not_inferred",
    }
    if not marker_present:
        result["reason"] = "missing_app_surface_marker"
    elif invalid:
        result["reason"] = "invalid_timestamp_pairs"
    elif duplicates:
        result["reason"] = "duplicate_timestamps"
    elif len(timestamps) < 2:
        result["reason"] = "insufficient_samples"
    else:
        span_ns = timestamps[-1] - timestamps[0]
        if span_ns <= 0:
            result["reason"] = "zero_timestamp_span"
        else:
            result.update(
                status="available",
                span_seconds=span_ns / 1_000_000_000,
                intervals_ms=interval_summary(intervals),
                frame_pacing=frame_pacing_summary(intervals),
            )
            result[rate_field] = (len(timestamps) - 1) * 1_000_000_000 / span_ns
    return result


def timestamp_semantics() -> dict:
    return {
        "basis": "public_rosen_source",
        "source": ROSEN_FPS_SOURCE,
        "first_column": "flushTime",
        "second_column": "presentTime",
        "device_source_version_verified": False,
        "limitation": "Public master may differ from the installed RenderService; activity is not inferred.",
    }


def analyze_fps(text: str) -> dict:
    """Default to the second (public Rosen presentTime) column, schema 2.

    Preserve both complete columns and the original ring. Never infer an action
    boundary from fpsClear, empty pre-action snapshots or a long-gap threshold.
    Device/source compatibility is unverified and explicit in every result.
    """
    pairs = fps_pairs(text)
    result = timestamp_column_summary(pairs, 1, marker_present=SURFACE in text, ring_pairs=pairs)
    result.update(
        schema_version=SCHEMA_VERSION,
        timestamp_semantics=timestamp_semantics(),
        ring_pairs_ns=pairs,
        all_present_intervals_ms=result["all_intervals_ms"],
        flush_stats=timestamp_column_summary(pairs, 0, marker_present=SURFACE in text, ring_pairs=pairs),
    )
    return result


def analyze_action_fps(
    text: str, input_ns: int | None, *, clock_monotonic_confirmed: bool = False,
) -> dict:
    """Describe candidates after a real native input boundary supplied by caller.

    Callers must obtain input_ns from this capture's PID-scoped native boundary
    record, and explicitly confirm that RS timestamps share CLOCK_MONOTONIC.
    This is a lower bound, not proof of a continuous animation or end boundary.
    Unconfirmed clocks never filter. Original ring metadata is inherited.
    """
    all_fps = analyze_fps(text)
    result = {
        "schema_version": SCHEMA_VERSION,
        "status": "inconclusive",
        "reason": "clock_monotonic_not_confirmed",
        "clock_monotonic_confirmed": clock_monotonic_confirmed is True,
        "input_monotonic_ns": input_ns if type(input_ns) is int and 0 < input_ns <= 2**64 - 1 else None,
        "all_fps": all_fps,
        "candidate_fps": None,
        "candidate_pairs_ns": None,
        "unattributed_pairs_ns": None,
        "ring_slot_count": all_fps["ring_slot_count"],
        "zero_slots": all_fps["zero_slots"],
        "ring_buffer_truncated_potential": all_fps["ring_buffer_truncated_potential"],
        "activity": "not_inferred",
        "scope": "Confirmed flush lower bound only; no animation or action end is inferred.",
    }
    if clock_monotonic_confirmed is not True:
        return result
    if result["input_monotonic_ns"] is None:
        result["reason"] = "missing_or_invalid_native_input_timestamp"
        return result
    if all_fps["reason"] in ("missing_app_surface_marker", "invalid_timestamp_pairs"):
        result["reason"] = all_fps["reason"]
        return result
    pairs = fps_pairs(text)
    nonempty = [pair for pair in pairs if valid_fps_pair(pair)]
    candidates = [pair for pair in nonempty if pair[0] >= input_ns]
    result["candidate_pairs_ns"] = candidates
    result["unattributed_pairs_ns"] = [pair for pair in nonempty if pair[0] < input_ns]
    candidate_fps = timestamp_column_summary(candidates, 1, marker_present=True, ring_pairs=pairs)
    candidate_fps.update(
        schema_version=SCHEMA_VERSION,
        timestamp_semantics=timestamp_semantics(),
        all_present_intervals_ms=candidate_fps["all_intervals_ms"],
        flush_stats=timestamp_column_summary(candidates, 0, marker_present=True, ring_pairs=pairs),
    )
    result["candidate_fps"] = candidate_fps
    result["status"] = "available_candidates" if candidate_fps["status"] == "available" else "inconclusive"
    result["reason"] = "confirmed_flush_lower_bound_candidates_only" if candidate_fps["status"] == "available" else candidate_fps["reason"]
    return result


def numeric_fields(text: str, allowed: set[str]) -> dict | None:
    """Accept only a complete, known list of anonymous numeric assignments."""
    fields = {}
    for token in text.split():
        match = re.fullmatch(rf"([A-Za-z_][A-Za-z_0-9]*)=({NUMBER})", token)
        if not match or match[1] not in allowed or match[1] in fields:
            return None
        key, value = match.groups()
        if value == "none":
            fields[key] = None
        elif value in ("true", "false"):
            fields[key] = value == "true"
        else:
            fields[key] = float(value) if "." in value else int(value)
    return fields if fields else None


def parse_perf_body(body: str) -> dict | None:
    if body.startswith("FeihongFontInit "):
        background = FONT_INIT_BACKGROUND.fullmatch(body)
        match = background or FONT_INIT_OWNER.fullmatch(body)
        if not match:
            return None
        values = {key: float(value) if key == "bootstrap_ms" else int(value)
                  for key, value in match.groupdict().items()}
        if not math.isfinite(values["bootstrap_ms"]) or values["bootstrap_ms"] < 0:
            return None
        if background and values["published"] > values["success"]:
            return None
        values["background"] = int(background is not None)
        return {"kind": "font_init", "values": values}
    if body.startswith("Native input boundary "):
        match = NATIVE_INPUT_BOUNDARY.fullmatch(body)
        if not match:
            return None
        values = {key: int(value) for key, value in match.groupdict().items()}
        if not 0 < values["monotonic_ns"] <= 2**64 - 1:
            return None
        return {"kind": "native_input_boundary", "values": values}
    if body.startswith("UI QoS:"):
        match = UI_QOS.fullmatch(body)
        if not match:
            return None
        values = {key: int(value) for key, value in match.groupdict().items()}
        if any(not -(2**31) <= value < 2**31 for value in values.values()):
            return None
        return {"kind": "ui_qos", "values": values}
    if body.startswith("Native present "):
        match = re.fullmatch(r"Native present size=(\d+)x(\d+)\s+(.*)", body)
        if not match:
            return None
        fields = numeric_fields(match[3], NATIVE_KEYS | NATIVE_DETAIL_KEYS | NATIVE_CPU_KEYS)
        if fields is None:
            return None
        details = set(fields) & NATIVE_DETAIL_KEYS
        cpu_details = set(fields) & NATIVE_CPU_KEYS
        if set(fields) - NATIVE_DETAIL_KEYS - NATIVE_CPU_KEYS not in (
            NATIVE_KEYS - {"mapCopyMs"}, NATIVE_KEYS - {"copyMs"},
        ) or (details and details != NATIVE_DETAIL_KEYS):
            return None
        if cpu_details and (cpu_details != NATIVE_CPU_KEYS or details != NATIVE_DETAIL_KEYS or "mapCopyMs" not in fields):
            return None
        if any(type(value) not in (int, float) for key, value in fields.items() if key != "pixelCopyCpuMs"):
            return None
        if cpu_details:
            if any(type(fields[key]) is not int or fields[key] < 0 for key in ("pixelCopyCpuSamples", "strideBytes", "bufferUsage")):
                return None
            if fields["bufferUsage"] > 2**32 - 1:
                return None
            cpu_ms = fields["pixelCopyCpuMs"]
            if fields["pixelCopyCpuSamples"] == 0:
                if cpu_ms is not None:
                    return None
            elif type(cpu_ms) not in (int, float) or not math.isfinite(cpu_ms) or cpu_ms < 0:
                return None
        return {"kind": "native_present", "size": [int(match[1]), int(match[2])], "values": fields}
    if body.startswith("[feihong-perf] "):
        rest = body.removeprefix("[feihong-perf] ")
        matches = list(re.finditer(r"([a-z_]+)\[([^\[\]]+)\]", rest))
        groups = {}
        for match in matches:
            name = match[1]
            values = numeric_fields(match[2], DISTRIBUTION_KEYS)
            if name not in GROUPS or name in groups or values is None or set(values) != DISTRIBUTION_KEYS:
                return None
            groups[name] = values
        fields = numeric_fields(re.sub(r"[a-z_]+\[[^\[\]]+\]", "", rest), RUST_KEYS)
        if fields is None or set(fields) != RUST_KEYS or set(groups) != GROUPS:
            return None
        return {"kind": "rust_ui", "values": fields, "distributions": groups}
    return None


def anonymous_perf_logs(text: str, app_pid: int) -> list[tuple[str, dict]]:
    records = []
    for line in text.splitlines():
        match = LOG_HEADER.fullmatch(line.strip())
        if not match or int(match["pid"]) != app_pid:
            continue
        body = match["body"]
        if match["tag"] == "Feihong":
            if body.startswith("FeihongFontInit "):
                pass  # The explicitly enabled font callback uses native::log directly.
            elif body.startswith("FeihongPerf "):
                body = body.removeprefix("FeihongPerf ")
            else:
                continue
        record = parse_perf_body(body)
        if record is not None and record["kind"] == "native_input_boundary" and match["tag"] != "FeihongPerf":
            continue
        if record is not None and record["kind"] == "font_init" and (
            match["tag"] != "Feihong" or not match["body"].startswith("FeihongFontInit ")
        ):
            continue
        if record is not None:
            # Used only for snapshot subtraction; raw lines are never saved.
            identity = " ".join(line.split())
            records.append((hashlib.sha256(identity.encode()).hexdigest(), record))
    return records


def new_perf_logs(before: str, after: str, app_pid: int) -> list[dict]:
    previous = {fingerprint for fingerprint, _ in anonymous_perf_logs(before, app_pid)}
    seen = set()
    records = []
    for fingerprint, record in anonymous_perf_logs(after, app_pid):
        if fingerprint not in previous and fingerprint not in seen:
            records.append(record)
        seen.add(fingerprint)
    return records


def find_hdc(explicit: str | None) -> str:
    if explicit:
        return explicit
    if os.environ.get("HDC"):
        return os.environ["HDC"]
    if command := shutil.which("hdc"):
        return command
    candidate = Path(os.environ.get("DEVECO_STUDIO_CONTENTS", "/Applications/DevEco-Studio.app/Contents")) / "sdk/default/openharmony/toolchains/hdc"
    if candidate.is_file():
        return str(candidate)
    raise DiagnosticError("hdc_not_found_use_explicit_hdc_path")


def device_shell(hdc: str, device: str, command: str, stage: str) -> str:
    try:
        run = subprocess.run([hdc, "-t", device, "shell", command], capture_output=True, timeout=15, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise DiagnosticError(f"{stage}_command_unavailable") from None
    if run.returncode:
        raise DiagnosticError(f"{stage}_command_failed")
    if len(run.stdout) > MAX_INPUT_BYTES:
        raise DiagnosticError(f"{stage}_output_too_large")
    return run.stdout.decode("utf-8", errors="replace")


def capture_live(
    device: str, duration: float, app_pid: int, hdc: str, *,
    input_ns: int | None = None, clock_monotonic_confirmed: bool = False,
) -> dict:
    # Each command is scoped to the known app surface or PID/tag. No global
    # counter/log reset, system trace, app launch, or device setting is performed.
    log_command = f"hilog -x -P {app_pid} -T FeihongPerf,Feihong"
    before = device_shell(hdc, device, log_command, "perf_log_before")
    cleared = device_shell(hdc, device, f"hidumper -s RenderService -a '{SURFACE} fpsClear'", "app_fps_clear")
    if re.search(r"(?:unknown|invalid|not found|failed|error)", cleared, re.IGNORECASE):
        raise DiagnosticError("app_fps_clear_not_supported")
    started = time.monotonic()
    time.sleep(duration)
    fps_text = device_shell(hdc, device, f"hidumper -s RenderService -a '{SURFACE} fps'", "app_fps_read")
    elapsed = time.monotonic() - started
    after = device_shell(hdc, device, log_command, "perf_log_after")
    result = analyze_fps(fps_text)
    records = new_perf_logs(before, after, app_pid)
    report = {
        "schema_version": SCHEMA_VERSION,
        "mode": "live",
        "capture_requested_seconds": duration,
        "capture_elapsed_seconds": elapsed,
        "app_pid": app_pid,
        "fps": result,
        "anonymous_perf_records": records,
        "anonymous_perf_record_count": len(records),
        "perf_log_status": "available" if records else "no_new_supported_anonymous_records",
        "raw_logs_saved": False,
    }
    if input_ns is not None or clock_monotonic_confirmed:
        report["action_fps"] = analyze_action_fps(
            fps_text, input_ns, clock_monotonic_confirmed=clock_monotonic_confirmed,
        )
    return report


def assess_continuous_gesture(report: dict, asserted: bool, min_fps: float) -> dict:
    """Assess an operator-declared benchmark; timestamps do not infer activity."""
    assessment = {
        "status": "not_assessed",
        "reason": "continuous_gesture_not_asserted",
        "operator_asserted_continuous_gesture": asserted,
        "benchmark_min_fps": min_fps,
        "required_coverage_ratio": 0.8,
        "coverage_ratio": None,
        "threshold_applied": False,
    }
    if not asserted:
        return assessment
    assessment["status"] = "inconclusive"
    fps = report.get("fps", {})
    if fps.get("schema_version") != SCHEMA_VERSION or fps.get("timestamp_column") != "second_present":
        assessment["reason"] = "second_column_schema_required"
        return assessment
    if fps.get("status") != "available":
        assessment["reason"] = "fps_unavailable"
        return assessment
    requested = report.get("capture_requested_seconds")
    span = fps.get("span_seconds")
    if not isinstance(requested, (int, float)) or isinstance(requested, bool) or not math.isfinite(requested) or requested <= 0:
        assessment["reason"] = "missing_capture_duration"
        return assessment
    if not isinstance(span, (int, float)) or isinstance(span, bool) or not math.isfinite(span) or span <= 0:
        assessment["reason"] = "invalid_timestamp_span"
        return assessment
    assessment["coverage_ratio"] = span / requested
    if fps.get("ring_buffer_truncated_potential") is not False:
        assessment["reason"] = "truncated_or_unknown_frame_ring"
        return assessment
    if assessment["coverage_ratio"] < assessment["required_coverage_ratio"]:
        assessment["reason"] = "insufficient_timestamp_coverage"
        return assessment
    observed = fps.get("actual_present_fps")
    if not isinstance(observed, (int, float)) or isinstance(observed, bool) or not math.isfinite(observed) or observed < 0:
        assessment["reason"] = "fps_unavailable"
        return assessment
    passed = observed >= min_fps
    assessment.update(
        status="pass" if passed else "fail",
        reason="meets_selected_min_fps" if passed else "below_selected_min_fps",
        threshold_applied=True,
    )
    return assessment


def report_exit_code(report: dict) -> int:
    status = report.get("assessment", {}).get("status")
    if status == "fail":
        return 1
    if status == "inconclusive":
        return 2
    return 0 if report.get("fps", {}).get("status") == "available" else 2


def write_report(path: Path, report: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as output:
            temporary = Path(output.name)
            os.chmod(temporary, 0o600)
            json.dump(report, output, ensure_ascii=False, indent=2, allow_nan=False)
            output.write("\n")
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def positive_pid(value: str) -> int:
    try:
        pid = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("app PID must be a positive integer") from None
    if not 0 < pid < 2**31:
        raise argparse.ArgumentTypeError("app PID must be a positive integer")
    return pid


def duration_seconds(value: str) -> float:
    try:
        duration = float(value)
    except ValueError:
        raise argparse.ArgumentTypeError("duration must be between 1 and 60 seconds") from None
    if not math.isfinite(duration) or not 1 <= duration <= 60:
        raise argparse.ArgumentTypeError("duration must be between 1 and 60 seconds")
    return duration


def minimum_fps(value: str) -> float:
    try:
        threshold = float(value)
    except ValueError:
        raise argparse.ArgumentTypeError("minimum FPS must be a finite positive number") from None
    if not math.isfinite(threshold) or threshold <= 0:
        raise argparse.ArgumentTypeError("minimum FPS must be a finite positive number")
    return threshold


def positive_monotonic_ns(value: str) -> int:
    if not re.fullmatch(r"\d{1,20}", value) or not 0 < int(value) <= 2**64 - 1:
        raise argparse.ArgumentTypeError("input timestamp must be a positive uint64 from a native input boundary")
    return int(value)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--fps-file", type=Path, help="offline RS dump for feihong-surfaceSurface")
    modes.add_argument("--device", help="explicit HDC device selector (live mode)")
    modes.add_argument("--self-test", action="store_true", help="run deterministic tests; no HDC")
    parser.add_argument("--output", type=Path, help="write only anonymous JSON, with mode 0600")
    parser.add_argument("--duration", type=duration_seconds, default=10.0, help="live gesture capture duration, 1–60 seconds (default: 10)")
    parser.add_argument("--app-pid", type=positive_pid, help="current app PID, required for live log filtering")
    parser.add_argument("--hdc", help="HDC executable path; otherwise HDC/PATH/DevEco default")
    parser.add_argument("--continuous-gesture", action="store_true", help="operator explicitly confirms continuous gestures for the entire live capture; enable optional assessment")
    parser.add_argument("--min-fps", type=minimum_fps, help="developer benchmark threshold (default: 30 only with --continuous-gesture); without gesture assertion no pass/fail")
    parser.add_argument("--input-monotonic-ns", type=positive_monotonic_ns, help="real timestamp from this capture's PID-scoped Native input boundary; adds separate action candidates")
    parser.add_argument("--confirm-clock-monotonic", action="store_true", help="explicitly confirm RS and native input timestamps share CLOCK_MONOTONIC; never inferred from a gap")
    args = parser.parse_args()
    if args.self_test:
        return 0 if unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Tests)).wasSuccessful() else 1
    if args.output is None:
        parser.error("--output is required")
    if args.fps_file and args.output.resolve() == args.fps_file.resolve():
        parser.error("--output must differ from --fps-file to preserve the evidence")
    if args.output.exists():
        parser.error("--output must be a new file to preserve historical evidence")
    if args.device and args.app_pid is None:
        parser.error("live mode requires --app-pid from the device debugging owner")
    if args.device and not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", args.device):
        parser.error("invalid device selector")
    assessment_requested = args.continuous_gesture or args.min_fps is not None
    selected_min_fps = args.min_fps if args.min_fps is not None else 30.0
    try:
        if args.fps_file:
            try:
                raw = args.fps_file.read_bytes()
            except OSError:
                raise DiagnosticError("fps_input_unreadable") from None
            if len(raw) > MAX_INPUT_BYTES:
                raise DiagnosticError("fps_input_too_large")
            report = {
                "schema_version": SCHEMA_VERSION,
                "mode": "offline",
                "fps": analyze_fps(raw.decode("utf-8", errors="replace")),
                "raw_logs_saved": False,
            }
            if args.input_monotonic_ns is not None or args.confirm_clock_monotonic:
                report["action_fps"] = analyze_action_fps(
                    raw.decode("utf-8", errors="replace"), args.input_monotonic_ns,
                    clock_monotonic_confirmed=args.confirm_clock_monotonic,
                )
        else:
            report = capture_live(
                args.device, args.duration, args.app_pid, find_hdc(args.hdc),
                input_ns=args.input_monotonic_ns, clock_monotonic_confirmed=args.confirm_clock_monotonic,
            )
        if assessment_requested:
            report["assessment"] = assess_continuous_gesture(report, args.continuous_gesture, selected_min_fps)
        write_report(args.output, report)
        print(json.dumps(report, ensure_ascii=False, allow_nan=False))
        return report_exit_code(report)
    except (DiagnosticError, OSError) as error:
        # Do not print command arguments, device IDs, raw output, or arbitrary
        # filesystem errors, which can contain information from the workstation.
        reason = str(error) if isinstance(error, DiagnosticError) else "report_output_unwritable"
        failure = {"schema_version": SCHEMA_VERSION, "status": "unavailable", "reason": reason, "raw_logs_saved": False}
        if assessment_requested:
            failure["assessment"] = assess_continuous_gesture(failure, args.continuous_gesture, selected_min_fps)
        if isinstance(error, DiagnosticError):
            try:
                write_report(args.output, failure)
            except OSError:
                pass
        print(json.dumps(failure))
        return 2


class Tests(unittest.TestCase):
    @staticmethod
    def dump(*pairs: tuple[int, int]) -> str:
        return SURFACE + " fps\n" + "\n".join(f"{begin}:{end}" for begin, end in pairs)

    def test_long_frame_counts_preserve_order_boundaries_and_idle_gaps(self):
        gaps = [16_000_000, 50_000_000, 50_000_001, 100_000_000,
                100_000_001, 16_000_000, 250_000_000]
        result = frame_pacing_summary(gaps)
        self.assertEqual(result["samples"], 7)
        self.assertEqual(result["activity"], "not_inferred")
        self.assertEqual(result["over_33_33_ms"]["count"], 5)
        self.assertEqual(result["over_33_33_ms"]["longest_run"], 4)
        self.assertEqual(result["over_50_ms"]["count"], 4)
        self.assertEqual(result["over_50_ms"]["longest_run"], 3)
        self.assertEqual(result["over_100_ms"]["count"], 2)
        self.assertAlmostEqual(result["over_100_ms"]["ratio"], 2 / 7)

    def test_invalid_or_missing_timestamps_never_generate_pacing_counts(self):
        for dump in [self.dump((0, 0)), self.dump((1, 2)),
                     self.dump((1, 4), (3, 4)), self.dump((2, 1), (3, 4))]:
            self.assertIsNone(analyze_fps(dump)["frame_pacing"])
        self.assertIsNone(frame_pacing_summary([])["over_50_ms"]["ratio"])

    def test_pacing_uses_every_sorted_interval_even_when_ring_is_full(self):
        report = analyze_fps(self.dump((1_300_000_000, 1_301_000_000),
                                       (1_000_000_000, 1_001_000_000),
                                       (1_016_000_000, 1_017_000_000)))
        self.assertTrue(report["ring_buffer_truncated_potential"])
        self.assertEqual(report["frame_pacing"]["samples"], 2)
        self.assertEqual(report["frame_pacing"]["over_100_ms"]["count"], 1)
        self.assertEqual(report["intervals_ms"]["max"], 284.)

    def test_circular_slots_sorted_and_real_span_used(self):
        report = analyze_fps(self.dump((1_400_000_000, 1_410_000_000), (0, 0), (1_000_000_000, 1_010_000_000), (1_200_000_000, 1_210_000_000)))
        self.assertEqual(report["frame_count"], 3)
        self.assertEqual(report["zero_slots"], 1)
        self.assertFalse(report["source_order_monotonic"])
        self.assertEqual(report["actual_present_fps"], 5.0)
        self.assertEqual(report["intervals_ms"]["median"], 200.0)

    def test_insufficient_samples_unavailable(self):
        for pairs in [((0, 0),), ((1, 2),)]:
            report = analyze_fps(self.dump(*pairs))
            self.assertEqual(report["reason"], "insufficient_samples")
            self.assertIsNone(report["actual_present_fps"])

    def test_duplicates_and_invalid_pairs_do_not_inflate_fps(self):
        self.assertEqual(analyze_fps(self.dump((1, 4), (3, 4)))["reason"], "duplicate_timestamps")
        self.assertEqual(analyze_fps(self.dump((2, 1), (3, 4)))["reason"], "invalid_timestamp_pairs")
        self.assertEqual(analyze_fps(self.dump((-1, 2), (3, 4)))["reason"], "invalid_timestamp_pairs")

    def test_long_intervals_are_retained_and_p95_uses_nearest_rank(self):
        report = analyze_fps(self.dump((1_000_000_000, 1_001_000_000), (1_100_000_000, 1_101_000_000), (4_000_000_000, 4_001_000_000)))
        self.assertAlmostEqual(report["actual_present_fps"], 2 / 3)
        self.assertEqual(report["intervals_ms"], {"samples": 2, "median": 1500.0, "p95": 2900.0, "max": 2900.0})

    def test_other_surface_is_unavailable(self):
        self.assertEqual(analyze_fps("otherSurface\n1:2\n3:4")["reason"], "missing_app_surface_marker")

    def test_second_column_long_frame_cannot_be_hidden_by_uniform_flush(self):
        result = analyze_fps(self.dump((1_000_000_000, 1_001_000_000),
                                       (1_016_000_000, 1_017_000_000),
                                       (1_032_000_000, 1_182_000_000), (0, 0)))
        self.assertEqual(result["schema_version"], 2)
        self.assertEqual(result["timestamp_column"], "second_present")
        self.assertFalse(result["timestamp_semantics"]["device_source_version_verified"])
        self.assertEqual(result["timestamp_semantics"]["first_column"], "flushTime")
        self.assertEqual(result["intervals_ms"]["p95"], 165.0)
        self.assertEqual(result["frame_pacing"]["over_100_ms"]["count"], 1)
        self.assertAlmostEqual(result["actual_present_fps"], 2 / .181)
        self.assertEqual(result["flush_stats"]["observed_flush_fps"], 62.5)
        self.assertEqual(result["flush_stats"]["intervals_ms"]["p95"], 16.)
        self.assertNotIn("actual_present_fps", result["flush_stats"])
        report = {"capture_requested_seconds": .18, "fps": result}
        self.assertEqual(assess_continuous_gesture(report, True, 30)["status"], "fail")

    def test_both_columns_retain_every_gap_and_validate_duplicates_independently(self):
        second_duplicate = analyze_fps(self.dump((1, 5), (3, 5), (0, 0)))
        self.assertIsNone(second_duplicate["actual_present_fps"])
        self.assertEqual(second_duplicate["duplicate_timestamps"], 1)
        self.assertEqual(second_duplicate["all_present_intervals_ms"], [0.])
        self.assertEqual(second_duplicate["flush_stats"]["status"], "available")
        first_duplicate = analyze_fps(self.dump((1, 3), (1, 5), (0, 0)))
        self.assertEqual(first_duplicate["status"], "available")
        self.assertEqual(first_duplicate["flush_stats"]["reason"], "duplicate_timestamps")
        self.assertEqual(first_duplicate["ring_pairs_ns"], [(1, 3), (1, 5), (0, 0)])

    def test_delayed_old_flush_is_unattributed_even_when_present_follows_input(self):
        pairs = ((900_000_000, 1_030_000_000), (1_000_000_000, 1_020_000_000),
                 (1_016_000_000, 1_040_000_000), (0, 0))
        result = analyze_action_fps(self.dump(*pairs), 1_000_000_000, clock_monotonic_confirmed=True)
        self.assertEqual(result["status"], "available_candidates")
        self.assertEqual(result["candidate_pairs_ns"], list(pairs[1:3]))
        self.assertEqual(result["unattributed_pairs_ns"], [pairs[0]])
        self.assertEqual(result["all_fps"]["ring_pairs_ns"], list(pairs))
        self.assertEqual(result["all_fps"]["all_present_intervals_ms"], [10., 10.])
        self.assertEqual(result["candidate_fps"]["all_present_intervals_ms"], [20.])
        self.assertEqual(result["candidate_fps"]["actual_present_fps"], 50.)
        self.assertEqual(result["candidate_fps"]["ring_slot_count"], 4)
        self.assertEqual(result["candidate_fps"]["zero_slots"], 1)
        self.assertFalse(result["candidate_fps"]["ring_buffer_truncated_potential"])
        self.assertFalse(result["ring_buffer_truncated_potential"])

    def test_action_candidates_preserve_long_gap_and_original_full_ring_flag(self):
        text = self.dump((1_000_000_000, 1_001_000_000), (1_016_000_000, 1_017_000_000),
                         (4_000_000_000, 4_001_000_000))
        result = analyze_action_fps(text, 1_016_000_000, clock_monotonic_confirmed=True)
        self.assertEqual(result["candidate_fps"]["intervals_ms"]["max"], 2984.)
        self.assertEqual(result["candidate_fps"]["frame_pacing"]["over_100_ms"]["count"], 1)
        self.assertEqual(len(result["unattributed_pairs_ns"]), 1)
        self.assertTrue(result["candidate_fps"]["ring_buffer_truncated_potential"])

    def test_unconfirmed_clock_never_assigns_old_or_candidate_slots(self):
        text = self.dump((1, 2), (3, 4), (0, 0))
        for confirmation in (False, None, 1, "true"):
            with self.subTest(confirmation=confirmation):
                result = analyze_action_fps(text, 3, clock_monotonic_confirmed=confirmation)
                self.assertEqual(result["status"], "inconclusive")
                self.assertEqual(result["reason"], "clock_monotonic_not_confirmed")
                self.assertIsNone(result["candidate_pairs_ns"])
                self.assertIsNone(result["unattributed_pairs_ns"])
                self.assertEqual(result["all_fps"]["ring_pairs_ns"], [(1, 2), (3, 4), (0, 0)])

    def test_action_requires_positive_uint64_real_input_value(self):
        text = self.dump((1, 2), (3, 4), (0, 0))
        for timestamp in (None, True, False, 0, -1, 1.0, "3", 2**64):
            with self.subTest(timestamp=timestamp):
                result = analyze_action_fps(text, timestamp, clock_monotonic_confirmed=True)
                self.assertEqual(result["reason"], "missing_or_invalid_native_input_timestamp")
                self.assertIsNone(result["candidate_fps"])
        self.assertEqual(analyze_action_fps(text, 3, clock_monotonic_confirmed=True)["reason"], "insufficient_samples")

    def test_action_does_not_make_duplicate_present_or_invalid_pairs_available(self):
        duplicate = analyze_action_fps(self.dump((1, 5), (3, 5), (0, 0)), 1, clock_monotonic_confirmed=True)
        self.assertEqual(duplicate["candidate_fps"]["reason"], "duplicate_timestamps")
        self.assertEqual(duplicate["status"], "inconclusive")
        invalid = analyze_action_fps(self.dump((2, 1), (3, 4)), 3, clock_monotonic_confirmed=True)
        self.assertEqual(invalid["reason"], "invalid_timestamp_pairs")
        self.assertIsNone(invalid["candidate_fps"])

    def test_native_input_boundary_is_complete_numeric_and_pid_tag_scoped(self):
        body = "Native input boundary action=0 monotonicNs=33136000000000"
        header = "10-11 14:20:54.321 4976 4977 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: "
        record = {"kind": "native_input_boundary", "values": {"action": 0, "monotonic_ns": 33136000000000}}
        self.assertEqual(parse_perf_body(body), record)
        self.assertEqual([r for _, r in anonymous_perf_logs(header + body, 4976)], [record])
        self.assertEqual(anonymous_perf_logs(header + body, 4977), [])
        self.assertEqual(anonymous_perf_logs(header.replace("FeihongPerf:", "OtherTag:") + body, 4976), [])
        self.assertEqual(anonymous_perf_logs(header.replace("moe.kiwi.FlectarMail", "another.bundle") + body, 4976), [])
        self.assertEqual(anonymous_perf_logs(header.replace("/FeihongPerf:", "/Feihong: FeihongPerf") + " " + body, 4976), [])
        self.assertEqual(parse_perf_body(body.replace("action=0", "action=1"))["values"]["action"], 1)
        self.assertEqual(parse_perf_body(body.replace("33136000000000", str(2**64 - 1)))["values"]["monotonic_ns"], 2**64 - 1)

    def test_native_input_boundary_rejects_extra_private_or_non_integer_fields(self):
        body = "Native input boundary action=0 monotonicNs=33136000000000"
        for suffix in (" x=1", " y=2", " text=secret", " password=1234", " extra=1", " action=1", " monotonicNs=5", " secret"):
            with self.subTest(suffix=suffix):
                self.assertIsNone(parse_perf_body(body + suffix))
        for action in ("2", "-1", "0.0", "none", "true", "00"):
            self.assertIsNone(parse_perf_body(body.replace("action=0", "action=" + action)))
        for timestamp in ("0", "-1", "1.5", "none", "true", "NaN", str(2**64)):
            self.assertIsNone(parse_perf_body(body.replace("33136000000000", timestamp)))

    def test_native_input_boundary_snapshot_dedup_preserves_distinct_events(self):
        header = "10-11 14:20:54.321 4976 4977 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: "
        old = header + "Native input boundary action=0 monotonicNs=1000"
        new = header + "Native input boundary action=1 monotonicNs=1001"
        records = new_perf_logs(old, "\n".join([old, new, new]), 4976)
        self.assertEqual(records, [{"kind": "native_input_boundary", "values": {"action": 1, "monotonic_ns": 1001}}])

    def test_font_init_background_success_failure_and_duplicate_publication(self):
        for success, published in ((1, 1), (1, 0), (0, 0)):
            body = f"FeihongFontInit background=1 success={success} published={published} bootstrap_ms=132.45"
            self.assertEqual(parse_perf_body(body), {"kind": "font_init", "values": {
                "background": 1, "success": success, "published": published, "bootstrap_ms": 132.45,
            }})

    def test_font_init_owner_ready_and_fallback_preserve_numeric_duration(self):
        for ready, duration in ((0, "202.00"), (1, "0.00"), (1, "1")):
            record = parse_perf_body(f"FeihongFontInit background=0 ready={ready} bootstrap_ms={duration}")
            self.assertEqual(record, {"kind": "font_init", "values": {
                "background": 0, "ready": ready, "bootstrap_ms": float(duration),
            }})

    def test_font_init_requires_complete_exact_source_schema_and_field_order(self):
        background = "FeihongFontInit background=1 success=1 published=1 bootstrap_ms=3.25"
        owner = "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"
        for body in (
            background.replace(" success=1", ""), background.replace(" published=1", ""),
            owner.replace(" ready=1", ""), owner.replace(" bootstrap_ms=0.05", ""),
            background + " ready=1", owner + " success=1 published=1",
            background.replace("success=1 published=1", "published=1 success=1"),
            owner.replace("background=0 ready=1", "ready=1 background=0"),
            background + " bootstrap_ms=3.25", owner.replace("ready=1", "ready=1  "),
        ):
            self.assertIsNone(parse_perf_body(body))

    def test_font_init_rejects_nonfinite_negative_and_non_numeric_durations(self):
        body = "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"
        for duration in ("NaN", "nan", "inf", "Infinity", "-1", "-0.0", "+1.0",
                         "1e999", "none", "true", ".5", "1.", "1/2", "private"):
            self.assertIsNone(parse_perf_body(body.replace("0.05", duration)))

    def test_font_init_rejects_invalid_flags_and_impossible_publication(self):
        background = "FeihongFontInit background=1 success=1 published=1 bootstrap_ms=3.25"
        owner = "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"
        for body, key, old in ((background, "background", "1"), (background, "success", "1"),
                               (background, "published", "1"), (owner, "ready", "1")):
            for flag in ("2", "-1", "1.0", "01", "true", "none"):
                self.assertIsNone(parse_perf_body(body.replace(f"{key}={old}", f"{key}={flag}")))
        self.assertIsNone(parse_perf_body(background.replace("success=1", "success=0")))

    def test_font_init_rejects_all_extra_fields_text_paths_and_coordinates(self):
        for body in ("FeihongFontInit background=1 success=1 published=1 bootstrap_ms=3.25",
                     "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"):
            for suffix in (" extra=1", " x=1 y=2", " path=/private/font.ttf", " text=synthetic",
                           " subject=synthetic", " password=1234", " private text", " "):
                self.assertIsNone(parse_perf_body(body + suffix))

    def test_font_init_accepts_only_own_pid_actual_native_logger_tag_and_body(self):
        body = "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"
        header = "10-11 14:20:54.321 4976 4977 E A0FF01/moe.kiwi.FlectarMail/Feihong: "
        record = {"kind": "font_init", "values": {"background": 0, "ready": 1, "bootstrap_ms": .05}}
        self.assertEqual([r for _, r in anonymous_perf_logs(header + body, 4976)], [record])
        self.assertEqual(anonymous_perf_logs(header + body, 4977), [])
        for invalid_header in (header.replace("/Feihong:", "/OtherTag:"),
                               header.replace("/Feihong:", "/FeihongPerf:"),
                               header.replace("moe.kiwi.FlectarMail", "another.bundle")):
            self.assertEqual(anonymous_perf_logs(invalid_header + body, 4976), [])
        self.assertEqual(anonymous_perf_logs(header + "FeihongPerf " + body, 4976), [])
        self.assertEqual(anonymous_perf_logs("Feihong: " + body, 4976), [])

    def test_font_init_snapshot_subtraction_preserves_new_identical_spans(self):
        header = "10-11 14:20:54.321 4976 4977 E A0FF01/moe.kiwi.FlectarMail/Feihong: "
        body = "FeihongFontInit background=0 ready=1 bootstrap_ms=0.05"
        old = header + body
        new = header.replace("54.321", "55.321") + body
        records = new_perf_logs(old, "\n".join([old, new, new]), 4976)
        self.assertEqual(records, [{"kind": "font_init", "values": {
            "background": 0, "ready": 1, "bootstrap_ms": .05,
        }}])
        self.assertFalse(any(key in records[0] for key in ("raw_log", "path", "pid", "text")))

    def test_offline_cli_uses_schema_two_preserves_old_flags_and_writes_private_json(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / "fps.txt", root / "report.json"
            source.write_text(self.dump((1_000_000_000, 1_001_000_000),
                                        (1_016_000_000, 1_117_000_000), (0, 0)))
            command = [sys.executable, str(Path(__file__).resolve()), "--fps-file", str(source),
                       "--output", str(output), "--min-fps", "30", "--input-monotonic-ns", "1000000000",
                       "--confirm-clock-monotonic"]
            run = subprocess.run(command, capture_output=True, timeout=5, check=False)
            self.assertEqual(run.returncode, 0, run.stderr.decode())
            report = json.loads(output.read_text())
            self.assertEqual(report["schema_version"], 2)
            self.assertAlmostEqual(report["fps"]["actual_present_fps"], 1 / .116)
            self.assertEqual(report["fps"]["intervals_ms"]["p95"], 116.)
            self.assertEqual(report["fps"]["flush_stats"]["intervals_ms"]["p95"], 16.)
            self.assertEqual(report["assessment"]["status"], "not_assessed")
            self.assertEqual(report["action_fps"]["status"], "available_candidates")
            self.assertFalse(report["action_fps"]["candidate_fps"]["ring_buffer_truncated_potential"])
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_offline_cli_refuses_to_replace_historical_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output = root / "fps.txt", root / "historical.json"
            source.write_text(self.dump((1, 2), (3, 4), (0, 0)))
            history = b'{"schema_version":1,"preserve":"original"}\n'
            output.write_bytes(history)
            run = subprocess.run([sys.executable, str(Path(__file__).resolve()),
                                  "--fps-file", str(source), "--output", str(output)],
                                 capture_output=True, timeout=5, check=False)
            self.assertEqual(run.returncode, 2)
            self.assertEqual(output.read_bytes(), history)
            self.assertEqual(run.stdout, b"")

    def test_native_logs_are_strictly_numeric_and_pid_scoped(self):
        body = "Native present size=2880x1920 attempts=6 presented=5 wallMs=2000.0 windowFps=2.50 avgMs=22.0 maxMs=30.0 requestMs=1.0 fenceMs=2.0 copyMs=18.0 flushMs=1.0"
        line = "10-11 14:20:54.321 4976 4977 I C0ff01/FeihongPerf: " + body
        self.assertEqual(len(anonymous_perf_logs(line, 4976)), 1)
        self.assertEqual(anonymous_perf_logs(line, 4977), [])
        self.assertIsNone(parse_perf_body(body + " unknown=private"))
        self.assertEqual(anonymous_perf_logs("FeihongPerf: unknown private text", 4976), [])
        actual_header = "10-11 13:32:03.856  4976  5224 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: "
        self.assertEqual(len(anonymous_perf_logs(actual_header + body, 4976)), 1)
        self.assertEqual(anonymous_perf_logs(actual_header.replace("moe.kiwi.FlectarMail", "another.bundle") + body, 4976), [])

    def test_rust_summary_schema_preserves_idle_and_missing_fps(self):
        body = "[feihong-perf] wall_ms=2000.000 active_ms=0.000 idle=true attempts=0 success=0 failed=0 active_success=0 wall_fps=none active_fps=none batches=0 events=0 "
        body += " ".join(f"{group}[samples=0 retained=0 total_ms=0.000 p50_ms=none p95_ms=none max_ms=none]" for group in sorted(GROUPS))
        record = parse_perf_body(body)
        self.assertIsNotNone(record)
        self.assertTrue(record["values"]["idle"])
        self.assertIsNone(record["values"]["wall_fps"])
        line = "10-11 14:20:54.321 4976 4976 E A0FF01/moe.kiwi.FlectarMail/Feihong: FeihongPerf " + body
        self.assertEqual(len(anonymous_perf_logs(line, 4976)), 1)
        self.assertEqual(anonymous_perf_logs(line.replace("FeihongPerf ", ""), 4976), [])

    def test_current_native_map_copy_schema_and_full_ring_warning(self):
        body = "Native present size=2880x1920 attempts=6 presented=5 wallMs=2000.0 windowFps=2.50 avgMs=22.0 maxMs=30.0 requestMs=1.0 fenceMs=2.0 mapCopyMs=18.0 flushMs=1.0"
        self.assertIsNotNone(parse_perf_body(body))
        self.assertTrue(analyze_fps(self.dump((1, 2), (3, 4)))["ring_buffer_truncated_potential"])
        self.assertFalse(analyze_fps(self.dump((0, 0), (1, 2), (3, 4)))["ring_buffer_truncated_potential"])

    @staticmethod
    def granular_native_body() -> str:
        return "Native present size=2880x1786 attempts=6 presented=6 wallMs=2000.0 windowFps=3.00 avgMs=81.90 maxMs=105.0 requestMs=0.2 fenceMs=0.1 mapCopyMs=80.0 flushMs=1.6 configMs=0.1 mapMs=74.8 pixelCopyMs=5.1 unmapMs=0.5"

    def test_native_granular_fields_preserve_combined_totals_and_old_identity(self):
        body = self.granular_native_body()
        detailed = parse_perf_body(body)
        previous = parse_perf_body(body.split(" configMs=")[0])
        self.assertEqual(detailed["kind"], "native_present")
        self.assertEqual(detailed["size"], previous["size"])
        self.assertEqual({key: value for key, value in detailed["values"].items() if key not in NATIVE_DETAIL_KEYS}, previous["values"])
        values = detailed["values"]
        self.assertAlmostEqual(values["mapCopyMs"], values["configMs"] + values["mapMs"] + values["pixelCopyMs"])
        self.assertAlmostEqual(values["flushMs"] - values["unmapMs"], 1.1)
        line = "10-11 14:20:54.321 4976 4976 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: " + body
        self.assertEqual(len(anonymous_perf_logs(line, 4976)), 1)
        self.assertEqual(len(new_perf_logs("", line + "\n" + line, 4976)), 1)

    def test_native_granular_fields_are_all_or_none(self):
        body = self.granular_native_body()
        self.assertIsNotNone(parse_perf_body(body.split(" configMs=")[0]))
        for key in NATIVE_DETAIL_KEYS:
            partial = re.sub(rf"\s{key}=\d+(?:\.\d+)?", "", body)
            self.assertIsNone(parse_perf_body(partial))

    def test_native_granular_fields_reject_unknown_and_non_numeric_values(self):
        body = self.granular_native_body()
        self.assertIsNone(parse_perf_body(body + " extraMs=1.0"))
        for value in ("none", "true", "false"):
            self.assertIsNone(parse_perf_body(body.replace("mapMs=74.8", f"mapMs={value}")))
        self.assertIsNone(parse_perf_body(body.replace("mapMs=74.8", "mapMs=-1.0")))

    def test_native_granular_fields_support_legacy_copy_schema(self):
        body = self.granular_native_body().replace("mapCopyMs=", "copyMs=")
        record = parse_perf_body(body)
        self.assertIsNotNone(record)
        self.assertEqual(record["values"]["copyMs"], 80.0)
        self.assertEqual(set(record["values"]) & NATIVE_DETAIL_KEYS, NATIVE_DETAIL_KEYS)

    @classmethod
    def cpu_native_body(cls) -> str:
        return cls.granular_native_body() + " pixelCopyCpuSamples=4 pixelCopyCpuMs=3.25 strideBytes=11520 bufferUsage=2147483650"

    def test_native_cpu_fields_preserve_valid_sample_mean_and_base_values(self):
        body = self.cpu_native_body()
        record = parse_perf_body(body)
        previous = parse_perf_body(self.granular_native_body())
        self.assertEqual({key: value for key, value in record["values"].items() if key not in NATIVE_CPU_KEYS}, previous["values"])
        # Four valid thread-clock samples need not equal six present attempts.
        self.assertEqual(record["values"]["pixelCopyCpuSamples"], 4)
        self.assertEqual(record["values"]["pixelCopyCpuMs"], 3.25)
        self.assertEqual(record["values"]["strideBytes"], 11520)
        self.assertEqual(record["values"]["bufferUsage"], 2147483650)
        line = "10-11 14:20:54.321 4976 4977 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: " + body
        self.assertEqual(len(anonymous_perf_logs(line, 4976)), 1)
        self.assertEqual(anonymous_perf_logs(line, 4977), [])
        self.assertEqual(len(new_perf_logs("", line + "\n" + line, 4976)), 1)

    def test_native_cpu_unknown_time_requires_zero_valid_samples(self):
        body = self.cpu_native_body()
        unknown = body.replace("pixelCopyCpuSamples=4 pixelCopyCpuMs=3.25", "pixelCopyCpuSamples=0 pixelCopyCpuMs=none")
        record = parse_perf_body(unknown)
        self.assertEqual(record["values"]["pixelCopyCpuSamples"], 0)
        self.assertIsNone(record["values"]["pixelCopyCpuMs"])
        self.assertIsNone(parse_perf_body(body.replace("pixelCopyCpuMs=3.25", "pixelCopyCpuMs=none")))
        self.assertIsNone(parse_perf_body(body.replace("pixelCopyCpuSamples=4", "pixelCopyCpuSamples=0")))

    def test_native_cpu_fields_require_complete_current_granular_schema(self):
        body = self.cpu_native_body()
        for key in NATIVE_CPU_KEYS | NATIVE_DETAIL_KEYS:
            with self.subTest(missing=key):
                self.assertIsNone(parse_perf_body(re.sub(rf"\s{key}=[^\s]+", "", body)))
        self.assertIsNone(parse_perf_body(body.replace("mapCopyMs=", "copyMs=")))
        self.assertIsNone(parse_perf_body(body + " pixelCopyCpuNs=1000"))

    def test_native_cpu_counters_are_unsigned_integers_and_usage_is_u32(self):
        body = self.cpu_native_body()
        for key, old in (("pixelCopyCpuSamples", "4"), ("strideBytes", "11520"), ("bufferUsage", "2147483650")):
            for invalid in ("none", "true", "false", "-1", "0.5"):
                with self.subTest(key=key, invalid=invalid):
                    self.assertIsNone(parse_perf_body(body.replace(f"{key}={old}", f"{key}={invalid}")))
            zero = body.replace(f"{key}={old}", f"{key}=0")
            if key == "pixelCopyCpuSamples":
                zero = zero.replace("pixelCopyCpuMs=3.25", "pixelCopyCpuMs=none")
            self.assertIsNotNone(parse_perf_body(zero))
        self.assertIsNotNone(parse_perf_body(body.replace("bufferUsage=2147483650", "bufferUsage=4294967295")))
        self.assertIsNone(parse_perf_body(body.replace("bufferUsage=2147483650", "bufferUsage=4294967296")))

    def test_native_cpu_time_rejects_non_numeric_and_negative_values(self):
        body = self.cpu_native_body()
        for invalid in ("true", "false", "-1.0", "NaN", "inf"):
            with self.subTest(invalid=invalid):
                self.assertIsNone(parse_perf_body(body.replace("pixelCopyCpuMs=3.25", f"pixelCopyCpuMs={invalid}")))
        self.assertIsNotNone(parse_perf_body(body.replace("pixelCopyCpuMs=3.25", "pixelCopyCpuMs=0.00")))

    def test_log_snapshot_subtraction_ignores_old_records(self):
        body = "Native present size=1x1 attempts=1 presented=1 wallMs=2000.0 windowFps=0.50 avgMs=1.0 maxMs=1.0 requestMs=0.0 fenceMs=0.0 copyMs=1.0 flushMs=0.0"
        old = "10-11 14:20:54.321 4976 4976 I C0ff01/FeihongPerf: " + body
        new = "10-11 14:20:56.321 4976 4976 I C0ff01/FeihongPerf: " + body
        self.assertEqual(len(new_perf_logs(old, old + "\n" + new, 4976)), 1)

    def test_repeated_snapshots_are_deduplicated_without_losing_new_equal_values(self):
        body = "Native present size=1x1 attempts=1 presented=1 wallMs=2000.0 windowFps=0.50 avgMs=1.0 maxMs=1.0 requestMs=0.0 fenceMs=0.0 copyMs=1.0 flushMs=0.0"
        old = "10-11 14:20:54.321 4976 4976 I C0ff01/FeihongPerf: " + body
        first = "10-11 14:20:56.321 4976 4976 I C0ff01/FeihongPerf: " + body
        second = "10-11 14:20:58.321 4976 4976 I C0ff01/FeihongPerf: " + body
        after = "\n".join([old, old.replace(" 4976 ", "  4976  "), first, first, second])
        self.assertEqual(len(new_perf_logs(old, after, 4976)), 2)

    def test_ui_qos_successful_set_preserves_unknown_before_level(self):
        record = parse_perf_body("UI QoS: interactive=1 setStatus=0 beforeStatus=-1 beforeLevel=-1 afterStatus=0 afterLevel=5")
        self.assertEqual(record, {"kind": "ui_qos", "values": {"interactive": 1, "setStatus": 0, "beforeStatus": -1, "beforeLevel": -1, "afterStatus": 0, "afterLevel": 5}})

    def test_ui_qos_successful_reset_accepts_get_unknown(self):
        record = parse_perf_body("UI QoS: interactive=0 setStatus=0 beforeStatus=0 beforeLevel=5 afterStatus=-1 afterLevel=-1")
        self.assertEqual(record["kind"], "ui_qos")
        self.assertEqual(record["values"]["setStatus"], 0)
        self.assertEqual(record["values"]["afterStatus"], -1)
        self.assertEqual(record["values"]["afterLevel"], -1)

    def test_ui_qos_set_error_is_recorded_without_inventing_success(self):
        record = parse_perf_body("UI QoS: interactive=1 setStatus=-1 beforeStatus=0 beforeLevel=4 afterStatus=0 afterLevel=4")
        self.assertEqual(record["kind"], "ui_qos")
        self.assertEqual(record["values"]["setStatus"], -1)
        self.assertEqual(record["values"]["afterLevel"], 4)

    def test_ui_qos_pid_and_complete_integer_schema_are_enforced(self):
        body = "UI QoS: interactive=1 setStatus=0 beforeStatus=-1 beforeLevel=-1 afterStatus=0 afterLevel=5"
        line = "10-11 14:20:54.321 4976 4977 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: " + body
        self.assertEqual(len(anonymous_perf_logs(line, 4976)), 1)
        self.assertEqual(anonymous_perf_logs(line, 4977), [])
        self.assertIsNone(parse_perf_body(body.replace("interactive=1", "interactive=2")))
        self.assertIsNone(parse_perf_body(body.replace("setStatus=0", "setStatus=0.1")))
        self.assertIsNone(parse_perf_body(body + " extra=1"))

    def test_ui_qos_uses_existing_timestamp_deduplication(self):
        body = "UI QoS: interactive=0 setStatus=0 beforeStatus=0 beforeLevel=5 afterStatus=-1 afterLevel=-1"
        old = "10-11 14:20:54.321 4976 4976 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: " + body
        new = "10-11 14:20:56.321 4976 4976 I A0FF01/moe.kiwi.FlectarMail/FeihongPerf: " + body
        self.assertEqual(len(new_perf_logs(old, "\n".join([old, old, new, new]), 4976)), 1)

    @staticmethod
    def assessment_report(rate=30.0, span=4.0, truncated=False, status="available") -> dict:
        return {"capture_requested_seconds": 5.0, "fps": {"schema_version": 2, "timestamp_column": "second_present", "status": status, "span_seconds": span, "actual_present_fps": rate, "ring_buffer_truncated_potential": truncated}}

    def test_assessment_rejects_legacy_first_column_or_unknown_schema(self):
        for key, value in (("schema_version", 1), ("timestamp_column", "first_flush")):
            report = self.assessment_report(rate=120.)
            report["fps"][key] = value
            result = assess_continuous_gesture(report, True, 30.)
            self.assertEqual(result["status"], "inconclusive")
            self.assertEqual(result["reason"], "second_column_schema_required")
            self.assertFalse(result["threshold_applied"])

    def test_continuous_assessment_pass_uses_selected_threshold_and_coverage_boundary(self):
        report = self.assessment_report()
        report["assessment"] = assess_continuous_gesture(report, True, 30.0)
        self.assertEqual(report["assessment"]["status"], "pass")
        self.assertEqual(report["assessment"]["coverage_ratio"], 0.8)
        self.assertEqual(report_exit_code(report), 0)
        self.assertEqual(assess_continuous_gesture(report, True, 60.0)["status"], "fail")

    def test_continuous_assessment_low_fps_fails_only_with_valid_context(self):
        report = self.assessment_report(rate=3.4)
        report["assessment"] = assess_continuous_gesture(report, True, 30.0)
        self.assertEqual(report["assessment"]["status"], "fail")
        self.assertTrue(report["assessment"]["operator_asserted_continuous_gesture"])
        self.assertTrue(report["assessment"]["threshold_applied"])
        self.assertEqual(report_exit_code(report), 1)

    def test_continuous_assessment_unavailable_is_inconclusive(self):
        report = self.assessment_report(status="unavailable")
        report["assessment"] = assess_continuous_gesture(report, True, 30.0)
        self.assertEqual(report["assessment"]["status"], "inconclusive")
        self.assertEqual(report["assessment"]["reason"], "fps_unavailable")
        self.assertEqual(report_exit_code(report), 2)

    def test_continuous_assessment_full_ring_is_inconclusive_even_at_high_rate(self):
        report = self.assessment_report(rate=120.0, truncated=True)
        report["assessment"] = assess_continuous_gesture(report, True, 30.0)
        self.assertEqual(report["assessment"]["status"], "inconclusive")
        self.assertFalse(report["assessment"]["threshold_applied"])
        self.assertEqual(report_exit_code(report), 2)

    def test_continuous_assessment_short_coverage_does_not_pass_or_fail(self):
        for rate in (3.4, 120.0):
            report = self.assessment_report(rate=rate, span=1.775)
            report["assessment"] = assess_continuous_gesture(report, True, 30.0)
            self.assertEqual(report["assessment"]["status"], "inconclusive")
            self.assertEqual(report["assessment"]["reason"], "insufficient_timestamp_coverage")
            self.assertAlmostEqual(report["assessment"]["coverage_ratio"], 0.355)
            self.assertEqual(report_exit_code(report), 2)

    def test_unknown_or_static_context_is_not_assessed_even_with_low_rate(self):
        report = self.assessment_report(rate=0.2)
        report["assessment"] = assess_continuous_gesture(report, False, 30.0)
        self.assertEqual(report["assessment"]["status"], "not_assessed")
        self.assertFalse(report["assessment"]["operator_asserted_continuous_gesture"])
        self.assertFalse(report["assessment"]["threshold_applied"])
        self.assertEqual(report_exit_code(report), 0)
        report.pop("assessment")
        self.assertEqual(report_exit_code(report), 0)

    def test_continuous_assessment_does_not_invent_offline_capture_duration(self):
        report = self.assessment_report()
        report.pop("capture_requested_seconds")
        assessment = assess_continuous_gesture(report, True, 30.0)
        self.assertEqual(assessment["status"], "inconclusive")
        self.assertEqual(assessment["reason"], "missing_capture_duration")


if __name__ == "__main__":
    sys.exit(main())
