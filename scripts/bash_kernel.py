import json
import os
import re
import subprocess
import sys
import tempfile


def sanitize(name):
    return re.sub(r"[^A-Za-z0-9_]", "_", name).upper()


class BashKernel:
    def __init__(self):
        self.cwd = os.getcwd()
        self.env = dict(os.environ)

    def execute(self, request):
        with tempfile.TemporaryDirectory(prefix="strata-bash-") as tmp:
            cwd_file = os.path.join(tmp, "cwd.txt")
            env_file = os.path.join(tmp, "env.bin")
            bridge_file = os.path.join(tmp, "bridges.txt")

            env = dict(self.env)
            env["STRATA_CWD_FILE"] = cwd_file
            env["STRATA_ENV_FILE"] = env_file
            env["STRATA_BRIDGE_FILE"] = bridge_file

            for key, value in request.get("inputs", {}).items():
                env[f"STRATA_INPUT_{sanitize(key)}"] = value

            script = f"""
strata_input() {{
  local name="$1"
  local safe="$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  local var="STRATA_INPUT_${{safe}}"
  printf '%s' "${{!var}}"
}}

strata_export() {{
  printf '%s=%s\\n' "$1" "$2" >> "$STRATA_BRIDGE_FILE"
}}

{request["source"]}
status=$?
pwd > "$STRATA_CWD_FILE"
env -0 > "$STRATA_ENV_FILE"
exit "$status"
"""

            completed = subprocess.run(
                ["bash", "--noprofile", "--norc", "-lc", script],
                cwd=self.cwd,
                env=env,
                text=True,
                capture_output=True,
            )

            if os.path.exists(cwd_file):
                next_cwd = open(cwd_file, "r", encoding="utf-8").read().strip()
                if next_cwd:
                    self.cwd = next_cwd

            previous_env = dict(self.env)
            next_env = dict(self.env)
            if os.path.exists(env_file):
                payload = open(env_file, "rb").read().split(b"\0")
                next_env = {}
                for item in payload:
                    if not item or b"=" not in item:
                        continue
                    key, value = item.split(b"=", 1)
                    next_env[key.decode("utf-8")] = value.decode("utf-8")
                self.env = next_env

            bridges = []
            for key, value in previous_env.items():
                if key in next_env and next_env.get(key) != value:
                    bridges.append(
                        {"kind": "environment", "key": key, "value": next_env[key]}
                    )
            for key, value in next_env.items():
                if key not in self.env:
                    bridges.append({"kind": "environment", "key": key, "value": value})

            if os.path.exists(bridge_file):
                for line in open(bridge_file, "r", encoding="utf-8"):
                    line = line.rstrip("\n")
                    if not line:
                        continue
                    name, _, value = line.partition("=")
                    bridges.append({"kind": "named_value", "name": name, "value": value})

            return {
                "output": completed.stdout.rstrip("\n"),
                "error_output": completed.stderr.rstrip("\n"),
                "exit_code": completed.returncode,
                "bridges": bridges,
            }


kernel = BashKernel()

for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    message = json.loads(line)
    if message.get("command") == "shutdown":
        break
    response = kernel.execute(message)
    print(json.dumps(response), flush=True)
