import contextlib
import io
import json
import sys
import traceback
from pathlib import Path


class StrataBridge:
    def __init__(self, inputs):
        self.inputs = inputs
        self.bridges = []
        self.displays = []

    def export(self, name, value):
        self.bridges.append(
            {"kind": "named_value", "name": str(name), "value": str(value)}
        )

    def input(self, name, default=None):
        return self.inputs.get(str(name), default)

    def artifact(self, name, path):
        self.bridges.append(
            {"kind": "artifact", "name": str(name), "path": str(path)}
        )

    def display(self, *objects):
        for obj in objects:
            payload = _display_payload(obj)
            if payload is not None:
                self.displays.append(payload)


STATE = {"__builtins__": __builtins__}


def _coerce_mime_value(mime, value):
    if value is None:
        return None
    if isinstance(value, bytes):
        if mime in {"image/png", "image/jpeg", "application/pdf"}:
            import base64

            return base64.b64encode(value).decode("ascii")
        return value.decode("utf-8", errors="replace")
    if isinstance(value, Path):
        return str(value)
    return str(value)


def _display_payload(obj):
    if obj is None:
        return {"data": {"text/plain": "None"}, "metadata": {}}

    plain_fallback = repr(obj)

    bundle_method = getattr(obj, "_repr_mimebundle_", None)
    if callable(bundle_method):
        bundle = bundle_method(include=None, exclude=None)
        metadata = {}
        if isinstance(bundle, tuple):
            data, metadata = bundle
        else:
            data = bundle
        if isinstance(data, dict):
            normalized = {}
            for mime, value in data.items():
                coerced = _coerce_mime_value(mime, value)
                if coerced is not None:
                    normalized[str(mime)] = coerced
            if normalized:
                normalized.setdefault("text/plain", plain_fallback)
                return {"data": normalized, "metadata": metadata or {}}

    for mime, method_name in [
        ("image/png", "_repr_png_"),
        ("image/jpeg", "_repr_jpeg_"),
        ("image/svg+xml", "_repr_svg_"),
        ("text/html", "_repr_html_"),
        ("text/markdown", "_repr_markdown_"),
        ("text/plain", "_repr_pretty_"),
    ]:
        method = getattr(obj, method_name, None)
        if callable(method):
            value = method()
            coerced = _coerce_mime_value(mime, value)
            if coerced:
                payload = {mime: coerced}
                payload.setdefault("text/plain", plain_fallback)
                return {"data": payload, "metadata": {}}

    if isinstance(obj, (str, int, float, bool, Path)):
        return {"data": {"text/plain": str(obj)}, "metadata": {}}

    return {"data": {"text/plain": plain_fallback}, "metadata": {}}


def execute(request):
    output = io.StringIO()
    errors = io.StringIO()
    bridge = StrataBridge(request.get("inputs", {}))
    STATE["strata"] = bridge
    STATE["display"] = bridge.display
    exit_code = 0

    try:
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            exec(request["source"], STATE, STATE)
    except Exception:
        exit_code = 1
        traceback.print_exc(file=errors)

    return {
        "output": output.getvalue().rstrip("\n"),
        "error_output": errors.getvalue().rstrip("\n"),
        "exit_code": exit_code,
        "bridges": bridge.bridges,
        "displays": bridge.displays,
    }


for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    message = json.loads(line)
    if message.get("command") == "shutdown":
        break
    response = execute(message)
    print(json.dumps(response), flush=True)
