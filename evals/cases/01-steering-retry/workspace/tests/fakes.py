"""Deterministic transport fakes for the fixture tests."""

from service_client import ServiceResponse


class ScriptedTransport:
    """Return scripted responses while recording every attempted operation."""

    def __init__(self, responses: list[ServiceResponse]) -> None:
        self._responses = list(responses)
        self.calls: list[tuple[str, str]] = []

    def send(self, operation: str, token: str) -> ServiceResponse:
        """Record and return the next scripted response."""

        self.calls.append((operation, token))
        if not self._responses:
            raise AssertionError("the client made more requests than scripted")
        return self._responses.pop(0)
