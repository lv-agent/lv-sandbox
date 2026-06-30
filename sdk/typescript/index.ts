/**
 * lvsandbox — TypeScript/JS client for lv-sandbox.
 *
 * Zero dependencies (Node 18+ built-in `fetch`).
 *
 * @example
 * ```ts
 * import { LvSandbox } from "lvsandbox";
 * const sandbox = new LvSandbox({ baseUrl: "http://127.0.0.1:8080" });
 * const result = await sandbox.jobs.run(["/bin/echo", "hello"]);
 * console.log(result.stdout);
 * ```
 *
 * @license MIT
 */

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface LvSandboxOptions {
  baseUrl?: string;
  apiKey?: string;
}

export interface JobResult {
  job_id: string;
  status: string;
  exit_code: number | null;
  signal: number | null;
  stdout: string;
  stderr: string;
  duration_ms: number;
  timed_out: boolean;
  files?: FileMeta[];
}

export interface FileMeta {
  path: string;
  size: number;
  mime: string;
}

export interface SessionInfo {
  session_id: string;
  profile: string;
  created_at_secs: number;
  last_activity_secs: number;
  execs: number;
}

export interface FileEntry {
  name: string;
  size: number;
  is_dir: boolean;
}

export interface WorkerStatus {
  running_jobs: number;
  max_concurrent: number;
  uptime_secs: number;
  disk_watermark_ok: boolean;
}

export type OpenAiToolSchema = {
  type: "function";
  function: {
    name: string;
    description: string;
    parameters: {
      type: "object";
      properties: Record<string, unknown>;
      required: string[];
    };
  };
};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

export class LvSandboxError extends Error {
  statusCode: number;
  constructor(statusCode: number, message: string) {
    super(message);
    this.name = "LvSandboxError";
    this.statusCode = statusCode;
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

function nowMs(): number {
  return Date.now();
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

export class LvSandbox {
  readonly baseUrl: string;
  readonly apiKey: string;

  // Sub-resources
  readonly jobs: JobsClient;
  readonly sessions: SessionsClient;
  readonly snapshots: SnapshotsClient;
  readonly volumes: VolumesClient;

  constructor(options: LvSandboxOptions = {}) {
    this.baseUrl = (options.baseUrl ?? process.env["LVSANDBOX_URL"] ?? "http://127.0.0.1:8080").replace(/\/+$/, "");
    this.apiKey = options.apiKey ?? process.env["LVSANDBOX_API_KEY"] ?? "";

    this.jobs = new JobsClient(this);
    this.sessions = new SessionsClient(this);
    this.snapshots = new SnapshotsClient(this);
    this.volumes = new VolumesClient(this);
  }

  // -- low-level HTTP -------------------------------------------------------

  async _fetch(method: string, path: string, body?: unknown, isBinary?: boolean): Promise<Response> {
    const headers: Record<string, string> = {};
    if (this.apiKey) {
      headers["Authorization"] = `Bearer ${this.apiKey}`;
    }
    if (body !== undefined && !isBinary) {
      headers["Content-Type"] = "application/json";
    }
    const init: RequestInit = {
      method,
      headers,
      body: body !== undefined ? (isBinary ? (body as BodyInit) : JSON.stringify(body)) : undefined,
    };
    const url = `${this.baseUrl}${path}`;
    const resp = await fetch(url, init);
    return resp;
  }

  async _json(method: string, path: string, body?: unknown): Promise<any> {
    const resp = await this._fetch(method, path, body);
    const data = await resp.json();
    if (!resp.ok) {
      throw new LvSandboxError(resp.status, data?.error ?? resp.statusText);
    }
    return data;
  }

  async _bytes(method: string, path: string, body?: BodyInit): Promise<Uint8Array> {
    const resp = await this._fetch(method, path, body, true);
    if (!resp.ok) {
      const data = await resp.json().catch(() => ({ error: resp.statusText }));
      throw new LvSandboxError(resp.status, data?.error ?? resp.statusText);
    }
    const buf = await resp.arrayBuffer();
    return new Uint8Array(buf);
  }

  // -- high-level -----------------------------------------------------------

  /** Worker status */
  async status(): Promise<WorkerStatus> {
    return this._json("GET", "/api/v1/status");
  }

  /** List available profiles */
  async profiles(): Promise<string[]> {
    const data = await this._json("GET", "/api/v1/profiles");
    return data.profiles as string[];
  }

  /** Health check */
  async health(): Promise<any> {
    return this._json("GET", "/health");
  }

  /**
   * Execute Python code in a sandbox session.
   * Creates a temporary session if none provided.
   */
  async runPython(
    code: string,
    options: { sessionId?: string; timeout?: string; profile?: string } = {},
  ): Promise<{ result: JobResult; files: FileEntry[] }> {
    const { sessionId, timeout = "60s", profile = "python" } = options;
    let sid = sessionId;
    if (!sid) {
      sid = await this.sessions.create(profile);
    }
    await this.sessions.files.put(sid, "_run.py", code);
    const result = await this.sessions.exec(sid, ["/usr/bin/python3", "_run.py"], timeout, undefined, true);
    const files = await this.sessions.files.list(sid);
    if (!sessionId) {
      await this.sessions.delete(sid).catch(() => {});
    }
    return { result, files };
  }

  /** Returns OpenAI function-calling tool schema for `run_python`. */
  openaiToolSchema(): OpenAiToolSchema {
    return {
      type: "function",
      function: {
        name: "run_python",
        description:
          "Execute Python code in a secure sandbox. Returns the result (stdout/stderr) and a list of output files.",
        parameters: {
          type: "object",
          properties: {
            code: { type: "string", description: "Python code to execute" },
            timeout: { type: "string", description: "Timeout, e.g. '60s'", default: "60s" },
          },
          required: ["code"],
        },
      },
    };
  }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

class JobsClient {
  constructor(private c: LvSandbox) {}

  async run(
    argv: string[],
    opts: {
      profile?: string;
      timeout?: string;
      env?: Record<string, string>;
      stdin?: string;
      jobId?: string;
      pollInterval?: number;
      pollTimeout?: number;
    } = {},
  ): Promise<JobResult> {
    const { profile = "shell", timeout = "5s", env = {}, stdin, jobId, pollInterval = 0.1, pollTimeout = 300 } = opts;
    const body: any = {
      argv,
      profile_name: profile,
      timeout,
      custom_env: env,
      job_id: jobId ?? `job-${nowMs()}`,
    };
    if (stdin !== undefined) body["stdin"] = stdin;

    const { job_id } = await this.c._json("POST", "/api/v1/jobs", body);
    const deadline = nowMs() + pollTimeout * 1000;

    while (true) {
      const r = await this.c._json("GET", `/api/v1/jobs/${job_id}`);
      if (r["status"] !== "Running") {
        return this._toJobResult(r);
      }
      if (nowMs() > deadline) {
        throw new LvSandboxError(408, `job ${job_id} poll timeout`);
      }
      await sleep(pollInterval * 1000);
    }
  }

  private _toJobResult(data: any): JobResult {
    return {
      job_id: data["job_id"],
      status: data["status"],
      exit_code: data["exit_code"] ?? null,
      signal: data["signal"] ?? null,
      stdout: data["stdout"] ?? "",
      stderr: data["stderr"] ?? "",
      duration_ms: data["duration_ms"] ?? 0,
      timed_out: data["timed_out"] ?? false,
      files: data["files"],
    };
  }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

class SessionsClient {
  files: FilesClient;

  constructor(private c: LvSandbox) {
    this.files = new FilesClient(c);
  }

  async create(
    profile: string,
    opts: { env?: Record<string, string>; fromSnapshot?: string; volumes?: { name: string; mount: string }[] } = {},
  ): Promise<string> {
    const body: any = { profile_name: profile, env: opts.env ?? {}, volumes: opts.volumes ?? [] };
    if (opts.fromSnapshot) body["from_snapshot"] = opts.fromSnapshot;
    const data = await this.c._json("POST", "/api/v1/sessions", body);
    return data["session_id"];
  }

  async list(): Promise<SessionInfo[]> {
    const data = await this.c._json("GET", "/api/v1/sessions");
    return data["sessions"] as SessionInfo[];
  }

  async get(id: string): Promise<SessionInfo | null> {
    const resp = await this.c._fetch("GET", `/api/v1/sessions/${id}`);
    if (resp.status === 404) return null;
    const data = await resp.json();
    if (!resp.ok) throw new LvSandboxError(resp.status, data?.error);
    return data as SessionInfo;
  }

  async delete(id: string): Promise<void> {
    await this.c._json("DELETE", `/api/v1/sessions/${id}`);
  }

  async exec(
    id: string,
    argv: string[],
    timeout?: string,
    stdin?: string,
    listFiles?: boolean,
  ): Promise<JobResult> {
    const body: any = { argv, timeout: timeout ?? "5s" };
    if (stdin !== undefined) body["stdin"] = stdin;
    if (listFiles) body["list_files"] = true;

    const data = await this.c._json("POST", `/api/v1/sessions/${id}/exec`, body);
    return {
      job_id: id,
      status: data["status"],
      exit_code: data["exit_code"] ?? null,
      signal: data["signal"] ?? null,
      stdout: data["stdout"] ?? "",
      stderr: data["stderr"] ?? "",
      duration_ms: data["duration_ms"] ?? 0,
      timed_out: data["timed_out"] ?? false,
      files: data["files"],
    };
  }

  async snapshot(id: string): Promise<string> {
    const data = await this.c._json("POST", `/api/v1/sessions/${id}/snapshot`);
    return data["snapshot_id"];
  }
}

// ---------------------------------------------------------------------------
// Files (session-scoped)
// ---------------------------------------------------------------------------

class FilesClient {
  constructor(private c: LvSandbox) {}

  async list(sid: string, path?: string): Promise<FileEntry[]> {
    const qs = path ? `?path=${encodeURIComponent(path)}` : "";
    const data = await this.c._json("GET", `/api/v1/sessions/${sid}/files${qs}`);
    return data["entries"] as FileEntry[];
  }

  async get(sid: string, relPath: string): Promise<Uint8Array> {
    return this.c._bytes("GET", `/api/v1/sessions/${sid}/files/${encodePath(relPath)}`);
  }

  async put(sid: string, relPath: string, data: string | Uint8Array): Promise<void> {
    const body = typeof data === "string" ? new TextEncoder().encode(data) : data;
    await this.c._bytes("PUT", `/api/v1/sessions/${sid}/files/${encodePath(relPath)}`, body as BodyInit);
  }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

class SnapshotsClient {
  constructor(private c: LvSandbox) {}

  async list(): Promise<string[]> {
    const data = await this.c._json("GET", "/api/v1/snapshots");
    return data["snapshots"] as string[];
  }

  async delete(id: string): Promise<void> {
    await this.c._json("DELETE", `/api/v1/snapshots/${id}`);
  }
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

class VolumesClient {
  constructor(private c: LvSandbox) {}

  async create(name: string): Promise<void> {
    await this.c._json("POST", "/api/v1/volumes", { name });
  }

  async list(): Promise<string[]> {
    const data = await this.c._json("GET", "/api/v1/volumes");
    return data["volumes"] as string[];
  }

  async delete(name: string): Promise<void> {
    await this.c._json("DELETE", `/api/v1/volumes/${encodeURIComponent(name)}`);
  }
}

// ---------------------------------------------------------------------------
// Utils
// ---------------------------------------------------------------------------

/** Encode path segments so `/` is not interpreted as route separator. */
function encodePath(p: string): string {
  return p.split("/").map(encodeURIComponent).join("/");
}
