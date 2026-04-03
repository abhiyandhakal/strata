import readline from "node:readline";
import vm from "node:vm";

const context = vm.createContext({
  JSON,
  Math,
  Date,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
});

function formatValue(value) {
  if (typeof value === "string") {
    return value;
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function makeBridge(inputs, bridges) {
  return {
    export(name, value) {
      bridges.push({
        kind: "named_value",
        name: String(name),
        value: formatValue(value),
      });
    },
    input(name, fallback = null) {
      const key = String(name);
      return Object.prototype.hasOwnProperty.call(inputs, key)
        ? inputs[key]
        : fallback;
    },
    artifact(name, path) {
      bridges.push({
        kind: "artifact",
        name: String(name),
        path: String(path),
      });
    },
  };
}

async function execute(request) {
  const output = [];
  const errors = [];
  const bridges = [];
  const sandboxConsole = {
    log: (...values) => output.push(values.map(formatValue).join(" ")),
    error: (...values) => errors.push(values.map(formatValue).join(" ")),
    warn: (...values) => errors.push(values.map(formatValue).join(" ")),
  };

  context.console = sandboxConsole;
  context.strata = makeBridge(request.inputs ?? {}, bridges);

  let source = request.source;
  if (request.language === "typescript") {
    if (typeof Bun === "undefined" || !Bun.Transpiler) {
      return {
        output: "",
        error_output: "TypeScript execution requires Bun runtime",
        exit_code: 1,
        bridges,
      };
    }
    source = new Bun.Transpiler({ loader: "ts" }).transformSync(source);
  }

  let exitCode = 0;
  try {
    const script = new vm.Script(source, {
      filename: `${request.cell_id ?? "cell"}.${request.language === "typescript" ? "ts" : "js"}`,
    });
    const result = script.runInContext(context);
    if (result && typeof result.then === "function") {
      await result;
    }
  } catch (error) {
    exitCode = 1;
    errors.push(error?.stack ?? String(error));
  }

  return {
    output: output.join("\n"),
    error_output: errors.join("\n"),
    exit_code: exitCode,
    bridges,
  };
}

const reader = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});

for await (const raw of reader) {
  const line = raw.trim();
  if (!line) {
    continue;
  }
  const message = JSON.parse(line);
  if (message.command === "shutdown") {
    reader.close();
    process.exit(0);
    break;
  }
  const response = await execute(message);
  process.stdout.write(JSON.stringify(response) + "\n");
}
