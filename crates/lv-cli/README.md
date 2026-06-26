# lvs — lv-sandbox CLI

Command-line client for [lv-sandbox](../../README.md): jobs, sessions, files,
snapshots, volumes.

## Build & run

```bash
cargo build -p lv-cli
./target/debug/lvs --url http://127.0.0.1:8080 status
```

Global flags: `--url` / `LVSANDBOX_URL` (default `http://127.0.0.1:8080`),
`--api-key` / `LVSANDBOX_API_KEY`.

## Commands

```bash
lvs status
lvs profiles
lvs jobs run --profile shell -- /bin/echo hi        # one-shot (exits with the job's code)

lvs sessions ls
lvs sessions new --profile shell                     # → prints session id
lvs sessions new --volume data:volumes/data         # mount a volume
lvs exec <id> -- /bin/sh -c 'echo hi > f.txt'        # session exec
lvs sessions rm <id>

lvs files put <id> run.sh ./local.sh                # upload
lvs files get <id> out.txt                          # download to stdout
lvs files get <id> out.txt --out ./local.txt        # download to file
lvs files ls <id>                                   # list workspace root

lvs snapshots ls
lvs snapshots rm <id>
lvs volumes ls
lvs volumes new data
lvs volumes rm data
```

`jobs run` and `exec` print the task's stdout to stdout and exit with the task's
exit code, so they compose in shell pipelines.
