#!/usr/bin/env python3
"""Redact and bound a Codex JSONL trajectory before it is published."""

from __future__ import annotations

import argparse
from collections.abc import Mapping, Sequence
import json
from pathlib import Path
import re
import sys
from typing import Any


DEFAULT_MAX_OUTPUT_CHARS = 4_000
HOME_REPLACEMENT = "$HOME"
WORKSPACE_REPLACEMENT = "$WORKSPACE"
OUTPUT_FIELDS = frozenset(
    {
        "aggregated_output",
        "output",
        "output_text",
        "result",
        "stderr",
        "stdout",
        "tool_output",
        "tool_result",
    }
)


class SanitizationError(ValueError):
    """Report invalid trajectory input or sanitizer configuration."""


def main(arguments: Sequence[str]) -> int:
    """Parse paths, sanitize one trajectory, and return a process status."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="raw Codex JSONL file")
    parser.add_argument("destination", type=Path, help="sanitized JSONL file")
    parser.add_argument(
        "--home",
        "--home-path",
        dest="home",
        type=Path,
        help="home path to redact; defaults to the current user's home",
    )
    parser.add_argument(
        "--max-output-chars",
        type=positive_integer,
        default=DEFAULT_MAX_OUTPUT_CHARS,
        help="maximum length for command and tool output fields",
    )
    namespace = parser.parse_args(arguments)

    try:
        sanitize_trajectory(
            namespace.source,
            namespace.destination,
            home_path=namespace.home,
            max_output_chars=namespace.max_output_chars,
        )
    except (OSError, SanitizationError) as error:
        print(f"sanitize error: {error}", file=sys.stderr)
        return 1
    return 0


def positive_integer(value: str) -> int:
    """Parse a positive integer for an argparse option."""

    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a positive integer") from error
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return parsed


def sanitize_trajectory(
    source: Path,
    destination: Path,
    *,
    home_path: Path | str | None = None,
    max_output_chars: int = DEFAULT_MAX_OUTPUT_CHARS,
) -> None:
    """Sanitize a JSONL trajectory and write it to ``destination``.

    The source is fully parsed before the destination is touched. This keeps a
    malformed or non-object record from replacing an already sanitized file.
    """

    limit = validate_output_limit(max_output_chars)
    events = read_events(Path(source))
    sanitizer = Sanitizer(home_path, limit)
    sanitized_events = [sanitizer.sanitize(event) for event in events]
    records = [sanitizer.metadata(len(events))]
    records.extend(sanitized_events)

    output = Path(destination)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        "".join(
            f"{json.dumps(record, ensure_ascii=False, separators=(',', ':'))}\n"
            for record in records
        ),
        encoding="utf-8",
    )


def read_events(source: Path) -> list[Mapping[str, Any]]:
    """Read nonblank JSONL records and reject malformed event input."""

    try:
        lines = source.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise SanitizationError(f"could not read source {source}: {error}") from error

    events: list[Mapping[str, Any]] = []
    for line_number, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise SanitizationError(
                f"invalid JSON at line {line_number}: {error.msg}"
            ) from error
        if not isinstance(value, Mapping):
            raise SanitizationError(
                f"JSONL record at line {line_number} must be a JSON object"
            )
        events.append(value)

    if not events:
        raise SanitizationError("source contains no JSON events")
    return events


def validate_output_limit(value: int) -> int:
    """Reject a non-positive or non-integer output limit."""

    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise SanitizationError("max_output_chars must be a positive integer")
    return value


class Sanitizer:
    """Apply deterministic path, identifier, and output normalization."""

    def __init__(self, home_path: Path | str | None, max_output_chars: int) -> None:
        self._max_output_chars = validate_output_limit(max_output_chars)
        self._thread_ids: dict[str, str] = {}
        self._truncated_output_count = 0
        self._home_patterns = home_patterns(home_path)

    def sanitize(
        self,
        value: Any,
        field_name: str | None = None,
        output_context: bool = False,
    ) -> Any:
        """Recursively sanitize JSON-compatible mappings, lists, and strings."""

        if isinstance(value, Mapping):
            nested_output_context = output_context or field_name_is_output(field_name)
            return {
                self.sanitize_key(key): self.sanitize(
                    item,
                    str(key),
                    output_context_for(str(key), nested_output_context),
                )
                for key, item in value.items()
            }
        if isinstance(value, list):
            return [self.sanitize(item, field_name, output_context) for item in value]
        if isinstance(value, str):
            redacted = self.redact_paths(value)
            if field_name_is_thread_id(field_name):
                return self.replace_thread_id(redacted)
            if field_name_is_output(field_name) or output_context:
                bounded = truncate_output(redacted, self._max_output_chars)
                if bounded != redacted:
                    self._truncated_output_count += 1
                return bounded
            return redacted
        return value

    def sanitize_key(self, key: Any) -> str:
        """Return a JSON object key after applying path redaction."""

        return self.redact_paths(str(key))

    def redact_paths(self, value: str) -> str:
        """Replace local home and evaluation workspace prefixes in text."""

        redacted = value
        redacted = DRIFTCTL_WORKSPACE_PATTERN.sub(WORKSPACE_REPLACEMENT, redacted)
        for pattern in self._home_patterns:
            redacted = pattern.sub(HOME_REPLACEMENT, redacted)
        return redacted

    def replace_thread_id(self, value: str) -> str:
        """Replace one provider thread identifier with a stable local label."""

        if not value:
            return value
        replacement = self._thread_ids.get(value)
        if replacement is None:
            replacement = f"thread-{len(self._thread_ids) + 1}"
            self._thread_ids[value] = replacement
        return replacement

    def metadata(self, event_count: int) -> dict[str, Any]:
        """Build the non-sensitive record prepended to the sanitized stream."""

        return {
            "event_count": event_count,
            "max_output_chars": self._max_output_chars,
            "record_type": "metadata",
            "redactions": {
                "home": HOME_REPLACEMENT,
                "temporary_run": f"{WORKSPACE_REPLACEMENT}/run",
                "temporary_workspace": WORKSPACE_REPLACEMENT,
                "thread_ids": "thread-N",
            },
            "schema_version": 1,
            "source": "codex",
            "truncated_output_fields": self._truncated_output_count,
            "type": "metadata",
        }


DRIFTCTL_WORKSPACE_PATTERN = re.compile(
    r"/tmp/driftctl-[^/\\\s\"'`]+/workspace(?![A-Za-z0-9_.-])"
)


def home_patterns(home_path: Path | str | None) -> tuple[re.Pattern[str], ...]:
    """Build path-boundary-aware patterns for supplied and current homes."""

    candidates = [Path.home()]
    if home_path is not None:
        candidates.append(Path(home_path).expanduser())
    patterns: list[re.Pattern[str]] = []
    seen: set[str] = set()
    for candidate in candidates:
        text = candidate.resolve(strict=False).as_posix().rstrip("/")
        if not text or text in seen:
            continue
        seen.add(text)
        patterns.append(re.compile(re.escape(text) + r"(?![A-Za-z0-9_.-])"))
    return tuple(patterns)


def field_name_is_thread_id(field_name: str | None) -> bool:
    """Recognize snake-case and camel-case provider thread ID fields."""

    if field_name is None:
        return False
    normalized = field_name.replace("-", "_").lower()
    return normalized in {"thread_id", "parent_thread_id"} or field_name == "threadId"


def field_name_is_output(field_name: str | None) -> bool:
    """Recognize fields where large command or tool output is expected."""

    return field_name is not None and field_name.lower() in OUTPUT_FIELDS


def output_field_in_context(field_name: str) -> bool:
    """Recognize text-bearing children inside an output object."""

    return field_name.lower() in {"content", "data", "text", "value"}


def output_context_for(field_name: str, parent_context: bool) -> bool:
    """Propagate output bounding into nested tool-result text fields."""

    return field_name_is_output(field_name) or (
        parent_context and output_field_in_context(field_name)
    )


def truncate_output(value: str, limit: int) -> str:
    """Keep both ends of long output while making the final string bounded."""

    if len(value) <= limit:
        return value
    omitted = len(value) - limit
    marker = f"[... omitted {omitted} chars ...]"
    if len(marker) >= limit:
        return marker[:limit]
    remaining = limit - len(marker)
    head_length = (remaining + 1) // 2
    tail_length = remaining - head_length
    tail = value[-tail_length:] if tail_length else ""
    return value[:head_length] + marker + tail


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
