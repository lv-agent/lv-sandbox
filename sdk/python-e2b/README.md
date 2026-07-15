# lvsandbox-e2b

E2B API-compatible shim over [lv-sandbox](../python). Reimplements the E2B SDK
interface surface on top of the lv-sandbox HTTP API.

> API-surface compatible, **not** wire-compatible. See
> `veps/cr-083-e2b-api-compatibility.md`.

```python
from lvsandbox_e2b import Sandbox

sb = Sandbox.create(template="base", timeout=60)
sb.filesystem.write("/hello.txt", b"hi")
print(sb.commands.run("cat /hello.txt").stdout)
sb.kill()
```
