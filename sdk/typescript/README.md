# lvsandbox

TypeScript/JS client for [lv-sandbox](https://github.com/lvtao/lv-sandbox).

Zero dependencies. Works with Node 18+ (native `fetch`).

## Install

```bash
npm install lvsandbox
```

## Quickstart

```ts
import { LvSandbox } from "lvsandbox";

const sandbox = new LvSandbox({ baseUrl: "http://127.0.0.1:8080" });

// One-shot job
const r = await sandbox.jobs.run(["/bin/echo", "hello"]);
console.log(r.stdout); // "hello\n"

// Execute Python in a sandbox
const { result, files } = await sandbox.runPython("print(1 + 1)");
console.log(result.stdout); // "2\n"
```

## API

### Jobs

```ts
const result = await sandbox.jobs.run(
  ["/bin/echo", "hello"],
  { profile: "shell", timeout: "5s" }
);
```

### Sessions

```ts
const sid = await sandbox.sessions.create("shell");
await sandbox.sessions.exec(sid, ["/bin/bash", "-c", "echo hi > out.txt"]);
const data = await sandbox.sessions.files.get(sid, "out.txt");
await sandbox.sessions.delete(sid);
```

### Python

```ts
const { result, files } = await sandbox.runPython(
  "print(42)",
  { timeout: "30s", profile: "python" }
);
```

### OpenAI Tool

```ts
const tool = sandbox.openaiToolSchema();
// Pass to OpenAI: { tools: [tool] }
```

## Config

```ts
const sandbox = new LvSandbox({
  baseUrl: "http://sandbox:8080",   // or LVSANDBOX_URL env
  apiKey:  "secret",                // or LVSANDBOX_API_KEY env
});
```

## License

MIT
