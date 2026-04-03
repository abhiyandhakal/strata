import contextlib
import io
import json
import sys
import traceback


class StrataBridge:
    def __init__(self, inputs):
        self.inputs = inputs
        self.bridges = []

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


STATE = {"__builtins__": __builtins__}


def execute(request):
    output = io.StringIO()
    errors = io.StringIO()
    bridge = StrataBridge(request.get("inputs", {}))
    STATE["strata"] = bridge
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
