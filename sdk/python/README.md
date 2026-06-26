# lvsandbox (Python SDK)

Thin Python client for [lv-sandbox](../../README.md): one-shot jobs, persistent
sessions (files / snapshots / volumes), streaming stdout, and worker
introspection. Mirrors the v0.3 HTTP API.

## Install

```bash
pip install -e sdk/python        # from the repo
# or, once published:  pip install lvsandbox
```

## Quick start

```python
from lvsandbox import Client

lv = Client("http://127.0.0.1:8080", api_key=None)

# one-shot job (blocks until done)
r = lv.jobs.run(["/bin/echo", "hello"], profile="shell", timeout="5s")
print(r.exit_code, r.stdout)

# stream live stdout
for ev in lv.jobs.stream(["/bin/sh", "-c", "for i in 1 2 3; do echo tick $i; done"]):
    if ev.type == "stdout":
        print(ev.stdout, end="")

# persistent session: files persist across exec calls
s = lv.sessions.create(profile="shell")
s.files.put("run.sh", b"echo from-session")
print(s.exec(["/bin/sh", "run.sh"]).stdout)

# snapshot / fork
snap = s.snapshot()
s2 = lv.sessions.create(profile="shell", from_snapshot=snap)

# volumes: persist across sessions
lv.volumes.create("data")
s3 = lv.sessions.create(profile="shell", volumes=[{"name": "data", "mount": "volumes/data"}])
```

## API surface

- `lv.jobs.run / .stream / .get / .cancel`
- `lv.sessions.create / .list / .get / .destroy`
- `session.exec / .exec_stream / .files.{put,get,list,delete} / .snapshot / .destroy`
- `lv.volumes.create / .list / .delete`
- `lv.status() / .profiles()`

Requires a running lv-sandbox server (v0.3+) and `httpx`.
