"""Attacks the server.py in the repository root, one per fixed weakness.

Runs a copy of it on a spare port against a scratch database, so it never
touches a real store. Needs what the server needs (fastapi, uvicorn,
websockets, bcrypt).

    python3 tests/server_test.py
"""
import asyncio, json, os, re, shutil, subprocess, sys, tempfile, time, urllib.request, urllib.error

import websockets

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8097
BASE = f"http://127.0.0.1:{PORT}"
failures = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def call(method, path, body=None, headers=None):
    req = urllib.request.Request(BASE + path, method=method,
                                 data=json.dumps(body).encode() if body is not None else None,
                                 headers={"content-type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, None


def scratch_server():
    d = tempfile.mkdtemp(prefix="ele-server-test-")
    src = open(os.path.join(ROOT, "server.py"), encoding="utf-8").read()
    src = re.sub(r"^_DATA_DIR = .*$", '_DATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")',
                 src, count=1, flags=re.M)
    os.makedirs(os.path.join(d, "data"))
    open(os.path.join(d, "server.py"), "w", encoding="utf-8").write(src)
    proc = subprocess.Popen([sys.executable, "-m", "uvicorn", "server:app", "--host", "127.0.0.1",
                             "--port", str(PORT), "--log-level", "warning"], cwd=d)
    for _ in range(50):
        try:
            urllib.request.urlopen(BASE + "/online", timeout=1)
            return d, proc
        except Exception:
            time.sleep(0.2)
    proc.kill()
    raise SystemExit("server did not start")


def account(name):
    call("POST", "/api/register", {"username": name, "pin": "4321"})
    return call("POST", "/api/login", {"username": name, "pin": "4321"})[1]["token"]


async def ws_checks(ta, tb, tc, cid):
    ws_url = f"ws://127.0.0.1:{PORT}/ws"

    # A non-member must not post into a group, over the websocket either.
    async with websockets.connect(f"{ws_url}/carol?token={tc}") as carol, \
               websockets.connect(f"{ws_url}/bob?token={tb}") as bob:
        await carol.send(json.dumps({"conversation_id": cid, "message": "intruder"}))
        reply = json.loads(await asyncio.wait_for(carol.recv(), 5))
        check("websocket: a non-member can't post into a group", "not a member" in reply.get("message", ""), reply)
        try:
            leaked = json.loads(await asyncio.wait_for(bob.recv(), 1))
        except asyncio.TimeoutError:
            leaked = None
        check("websocket: members never receive the intruder's message", leaked is None, leaked)

    # Closing an older connection must not take a newer one offline.
    old = await websockets.connect(f"{ws_url}/alice?token={ta}")
    new = await websockets.connect(f"{ws_url}/alice?token={ta}")
    await old.close()
    await asyncio.sleep(0.5)
    online = call("GET", f"/online?token={tb}")[1]["online"]
    check("websocket: closing an old connection keeps the newer one online", "alice" in online, online)
    await new.close()


def main():
    d, proc = scratch_server()
    try:
        # No default administrator.
        status, _ = call("POST", "/api/login", {"username": "admin", "pin": "1234"})
        check("no default admin/1234 account", status in (401, 403, 404), status)

        ta, tb, tc = account("alice"), account("bob"), account("carol")
        _, convo = call("POST", f"/api/conversation?token={ta}", {"members": ["bob"], "name": "family"})
        cid = convo["conversation_id"]

        # Same membership rule over HTTP.
        status, _ = call("POST", f"/api/send?token={tc}", {"conversation_id": cid, "message": "intruder"})
        check("http: a non-member can't post into a group", status == 403, status)

        # Machine sessions are for the local network: judge by the real caller.
        status, _ = call("POST", "/api/node-login", headers={"X-Real-IP": "203.0.113.9"})
        check("node session refused to a caller from the internet (via proxy)", status == 403, status)
        status, _ = call("POST", "/api/node-login", headers={"X-Real-IP": "192.168.1.20"})
        check("node session allowed to a caller on the LAN (via proxy)", status == 200, status)
        status, _ = call("POST", "/api/node-login")
        check("node session allowed on the machine itself", status == 200, status)

        # Settings: only the machine itself, only known keys.
        status, _ = call("POST", "/api/config", {"server_name": "x"}, headers={"X-Real-IP": "192.168.1.20"})
        check("settings can't be changed through the proxy", status == 403, status)
        status, _ = call("POST", "/api/config", {"evil": 1})
        check("unknown settings are refused", status == 400, status)
        status, _ = call("POST", "/api/config", {"server_name": "Test"})
        check("the machine itself can change a known setting", status == 200, status)

        # Presence and the connection log are for signed-in callers.
        status, body = call("GET", "/online")
        check("/online still answers 200 for reachability checks", status == 200, status)
        check("/online lists nobody to an anonymous caller", body == {"online": []}, body)
        status, _ = call("GET", "/api/log")
        check("/api/log needs a session", status == 401, status)

        asyncio.run(ws_checks(ta, tb, tc, cid))
    finally:
        proc.terminate()
        proc.wait(5)
        shutil.rmtree(d, ignore_errors=True)
    print(f"\n{len(failures)} failed" if failures else "\nall passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
