# ELE Messenger - server.py v1.3.0
import json, os, secrets, asyncio, uuid, sqlite3, base64
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request
from fastapi.responses import Response
import socket
from datetime import datetime, timedelta
from contextlib import contextmanager

BASE = os.path.dirname(os.path.abspath(__file__))
CONFIG_PATH = os.path.join(BASE, "config.json")
_DATA_DIR = "/var/lib/ele-messenger" if os.path.isdir("/var/lib/ele-messenger") else BASE
DB_PATH = os.path.join(_DATA_DIR, "ele.db")

def load_config():
    try:
        with open(CONFIG_PATH) as f:
            return json.load(f)
    except Exception:
        return {}

def get_db():
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    return conn

def convo_id(user_a, user_b):
    """Deterministic conversation ID for a pair of users."""
    return ":".join(sorted([user_a, user_b]))

def init_db():
    with get_db() as conn:
        # Accounts
        conn.execute("""
            CREATE TABLE IF NOT EXISTS accounts (
                username TEXT PRIMARY KEY,
                pin TEXT NOT NULL
            )
        """)

        # Conversations (group-chat ready)
        conn.execute("""
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                secret INTEGER NOT NULL DEFAULT 0
            )
        """)

        # Conversation members
        conn.execute("""
            CREATE TABLE IF NOT EXISTS conversation_members (
                conversation_id TEXT NOT NULL,
                username TEXT NOT NULL,
                PRIMARY KEY (conversation_id, username)
            )
        """)

        # New messages table with conversation_id
        conn.execute("""
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                from_user TEXT NOT NULL,
                message TEXT NOT NULL DEFAULT '',
                image_id TEXT,
                timestamp TEXT NOT NULL,
                secret INTEGER NOT NULL DEFAULT 0
            )
        """)

        # Pubkeys
        conn.execute("""
            CREATE TABLE IF NOT EXISTS pubkeys (
                username TEXT PRIMARY KEY,
                pubkey TEXT NOT NULL
            )
        """)

        conn.execute("CREATE INDEX IF NOT EXISTS idx_convo_members ON conversation_members(username)")

        # --- Migration from old schema ---
        # Check if old messages table has to_user column
        cols = [r[1] for r in conn.execute("PRAGMA table_info(messages)").fetchall()]
        if "to_user" in cols:
            print("Migrating old messages schema to conversation-based schema...")
            old_messages = conn.execute(
                "SELECT id, from_user, to_user, message, image_id, timestamp FROM messages"
            ).fetchall()

            # Recreate messages table without to_user
            conn.execute("ALTER TABLE messages RENAME TO messages_old")
            conn.execute("""
                CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id TEXT NOT NULL,
                    from_user TEXT NOT NULL,
                    message TEXT NOT NULL DEFAULT '',
                    image_id TEXT,
                    timestamp TEXT NOT NULL,
                    secret INTEGER NOT NULL DEFAULT 0
                )
            """)
            conn.execute("CREATE INDEX IF NOT EXISTS idx_messages_convo ON messages(conversation_id)")
            conn.execute("CREATE INDEX IF NOT EXISTS idx_messages_from ON messages(from_user)")

            # Migrate old messages
            for row in old_messages:
                cid = convo_id(row["from_user"], row["to_user"])
                # Ensure conversation exists
                conn.execute(
                    "INSERT OR IGNORE INTO conversations (id, secret) VALUES (?, 0)",
                    (cid,)
                )
                # Ensure members exist
                conn.execute(
                    "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                    (cid, row["from_user"])
                )
                conn.execute(
                    "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                    (cid, row["to_user"])
                )
                # Insert migrated message
                conn.execute(
                    "INSERT INTO messages (id, conversation_id, from_user, message, image_id, timestamp, secret) VALUES (?, ?, ?, ?, ?, ?, 0)",
                    (row["id"], cid, row["from_user"], row["message"], row["image_id"], row["timestamp"])
                )

            conn.execute("DROP TABLE messages_old")
            print("Migration complete.")

        conn.execute("CREATE INDEX IF NOT EXISTS idx_messages_convo ON messages(conversation_id)")
        conn.execute("CREATE INDEX IF NOT EXISTS idx_messages_from ON messages(from_user)")

        # Default admin account
        count = conn.execute("SELECT COUNT(*) FROM accounts").fetchone()[0]
        if count == 0:
            conn.execute(
                "INSERT INTO accounts (username, pin) VALUES (?, ?)",
                ("admin", "1234")
            )
        conn.commit()

app = FastAPI()
connected_clients: dict[str, WebSocket] = {}
connect_log: list[dict] = []
sessions: dict = {}
images: dict = {}  # in-memory only — transient

@app.on_event("startup")
async def startup_event():
    init_db()
    asyncio.create_task(cleanup_messages())

async def cleanup_messages():
    while True:
        await asyncio.sleep(3600)
        cutoff = (datetime.now() - timedelta(days=7)).isoformat()
        with get_db() as conn:
            conn.execute("DELETE FROM messages WHERE timestamp < ?", (cutoff,))
            conn.commit()

@app.post("/api/register")
async def register(data: dict):
    username = data.get("username", "").strip()
    pin = data.get("pin", "").strip()
    if not username or not pin:
        raise HTTPException(400, "username and pin required")
    if len(pin) != 4 or not pin.isdigit():
        raise HTTPException(400, "pin must be 4 digits")
    with get_db() as conn:
        existing = conn.execute("SELECT username FROM accounts WHERE username = ?", (username,)).fetchone()
        if existing:
            raise HTTPException(409, "username taken")
        conn.execute("INSERT INTO accounts (username, pin) VALUES (?, ?)", (username, pin))
        conn.execute("DELETE FROM accounts WHERE username = ?", ("admin",))
        conn.commit()
    return {"status": "ok"}

@app.post("/api/login")
async def login(data: dict):
    username = data.get("username", "").strip()
    pin = data.get("pin", "").strip()
    with get_db() as conn:
        row = conn.execute("SELECT pin FROM accounts WHERE username = ?", (username,)).fetchone()
    if not row or row["pin"] != pin:
        raise HTTPException(401, "invalid credentials")
    token = secrets.token_hex(16)
    sessions[token] = username
    return {"token": token, "username": username}

@app.get("/api/config")
async def get_config():
    return load_config()

@app.post("/api/config")
async def set_config(data: dict):
    cfg = load_config()
    cfg.update(data)
    with open(CONFIG_PATH, "w") as f:
        json.dump(cfg, f, indent=2)
    return cfg

@app.get("/api/accounts")
async def get_accounts(token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    with get_db() as conn:
        rows = conn.execute("SELECT username FROM accounts ORDER BY username").fetchall()
    return {"accounts": [r["username"] for r in rows]}

@app.get("/online")
async def online_users():
    return {"online": list(connected_clients.keys())}

@app.get("/api/log")
async def get_log():
    return {"log": connect_log[-50:]}

@app.post("/api/upload")
async def upload_image(data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    img_data = data.get("data", "")
    mime = data.get("mime", "image/jpeg")
    if len(img_data) > 3 * 1024 * 1024:
        raise HTTPException(413, "image too large")
    img_id = str(uuid.uuid4())
    images[img_id] = {"data": img_data, "mime": mime}
    return {"id": img_id}

@app.get("/api/image/{img_id}")
async def get_image(img_id: str, token: str = None, request: Request = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    if img_id not in images:
        raise HTTPException(404, "not found")
    img = images[img_id]
    data = img["data"]
    if "," in data:
        data = data.split(",", 1)[1]
    content = base64.b64decode(data)
    total = len(content)
    range_header = request.headers.get("range") if request else None
    if range_header:
        try:
            start, end = range_header.replace("bytes=", "").split("-")
            start = int(start)
            end = int(end) if end else total - 1
            end = min(end, total - 1)
            chunk = content[start:end+1]
            return Response(content=chunk, status_code=206, media_type=img["mime"],
                headers={"Content-Range": f"bytes {start}-{end}/{total}",
                         "Accept-Ranges": "bytes", "Content-Length": str(len(chunk))})
        except Exception:
            pass
    return Response(content=content, media_type=img["mime"],
        headers={"Accept-Ranges": "bytes", "Content-Length": str(total)})

@app.get("/api/history")
async def get_history(token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cutoff = (datetime.now() - timedelta(days=7)).isoformat()
    with get_db() as conn:
        # Get all conversation IDs this user is a member of
        convo_rows = conn.execute(
            "SELECT conversation_id FROM conversation_members WHERE username = ?",
            (username,)
        ).fetchall()
        convo_ids = [r["conversation_id"] for r in convo_rows]
        if not convo_ids:
            return {"history": []}
        placeholders = ",".join("?" * len(convo_ids))
        rows = conn.execute(f"""
            SELECT m.conversation_id, m.from_user, m.message, m.image_id, m.timestamp, m.secret,
                   c.secret as convo_secret
            FROM messages m
            JOIN conversations c ON c.id = m.conversation_id
            WHERE m.conversation_id IN ({placeholders}) AND m.timestamp > ?
            ORDER BY m.timestamp ASC
        """, (*convo_ids, cutoff)).fetchall()
        # Get other member for each convo (for client compatibility)
        member_map = {}
        for cid in convo_ids:
            members = conn.execute(
                "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid,)
            ).fetchall()
            member_map[cid] = [m["username"] for m in members]

    history = []
    for r in rows:
        members = member_map.get(r["conversation_id"], [])
        to_user = next((m for m in members if m != r["from_user"]), r["from_user"])
        history.append({
            "from": r["from_user"],
            "to": to_user,
            "conversation_id": r["conversation_id"],
            "message": r["message"],
            "image_id": r["image_id"],
            "timestamp": r["timestamp"],
            "secret": bool(r["secret"] or r["convo_secret"])
        })
    return {"history": history}

@app.get("/api/secret/{other_user}")
async def get_secret(other_user: str, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user)
    with get_db() as conn:
        row = conn.execute("SELECT secret FROM conversations WHERE id = ?", (cid,)).fetchone()
    return {"secret": bool(row["secret"]) if row else False}

@app.post("/api/secret/{other_user}")
async def set_secret(other_user: str, data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user)
    secret = 1 if data.get("secret") else 0
    with get_db() as conn:
        conn.execute(
            "INSERT INTO conversations (id, secret) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET secret = ?",
            (cid, secret, secret)
        )
        conn.execute("INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)", (cid, username))
        conn.execute("INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)", (cid, other_user))
        conn.commit()
    # Notify both parties of secret status change via WS if online
    payload = {"type": "secret_changed", "with": other_user, "secret": bool(secret)}
    if username in connected_clients:
        await connected_clients[username].send_json(payload)
    payload2 = {"type": "secret_changed", "with": username, "secret": bool(secret)}
    if other_user in connected_clients:
        await connected_clients[other_user].send_json(payload2)
    return {"status": "ok", "secret": bool(secret)}

@app.delete("/api/history/{other_user}")
async def delete_history(other_user: str, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user)
    with get_db() as conn:
        conn.execute("DELETE FROM messages WHERE conversation_id = ?", (cid,))
        conn.commit()
    payload = {"type": "chat_cleared", "with": username}
    if other_user in connected_clients:
        await connected_clients[other_user].send_json(payload)
    return {"status": "ok"}

@app.post("/api/pubkey")
async def set_pubkey(data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    pubkey = data.get("pubkey", "").strip()
    if not pubkey:
        raise HTTPException(400, "pubkey required")
    with get_db() as conn:
        conn.execute("INSERT OR REPLACE INTO pubkeys (username, pubkey) VALUES (?, ?)", (username, pubkey))
        conn.commit()
    return {"status": "ok"}

@app.get("/api/pubkey/{username}")
async def get_pubkey(username: str, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    with get_db() as conn:
        row = conn.execute("SELECT pubkey FROM pubkeys WHERE username = ?", (username,)).fetchone()
    if not row:
        raise HTTPException(404, "pubkey not found")
    return {"pubkey": row["pubkey"]}

# A plain POST alongside the websocket, so a client that only wants to SEND
# does not have to hold a socket open to do it.
#
# This exists because `connected_clients` holds exactly ONE socket per user:
# a second connection as the same person replaces the first, and closing it
# deletes the entry outright, so the original client silently stops receiving
# messages. The camera viewer's message card needed to send one line without
# evicting the chat app the family actually uses, which a socket cannot do
# and a request can. Delivery to anyone currently connected is unchanged --
# this writes the same row and pushes the same payload.
@app.post("/api/send")
async def send_message(data: dict, token: str = None):
    token = token or data.get("token")
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    message = (data.get("message") or "").strip()
    image_id = data.get("image_id")
    if not message and not image_id:
        raise HTTPException(400, "empty message")
    if len(message) > 4000:
        raise HTTPException(400, "message too long")
    secret = 1 if data.get("secret") else 0
    cid = data.get("conversation_id")
    target = data.get("to")
    ts = datetime.now().isoformat()

    with get_db() as conn:
        if cid:
            row = conn.execute("SELECT id FROM conversations WHERE id = ?", (cid,)).fetchone()
            if not row:
                raise HTTPException(404, "conversation not found")
            member_rows = conn.execute(
                "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid,)
            ).fetchall()
            members = [m["username"] for m in member_rows]
            if username not in members:
                raise HTTPException(403, "not a member")
        elif target:
            cid = convo_id(username, target)
            conn.execute("INSERT OR IGNORE INTO conversations (id, secret) VALUES (?, ?)",
                         (cid, secret))
            for u in (username, target):
                conn.execute(
                    "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                    (cid, u))
            members = [username, target]
        else:
            raise HTTPException(400, "need `to` or `conversation_id`")
        conn.execute(
            "INSERT INTO messages (conversation_id, from_user, message, image_id, timestamp, secret) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            (cid, username, message, image_id, ts, secret))
        conn.commit()

    payload = {"from": username, "message": message, "image_id": image_id,
               "secret": bool(secret), "conversation_id": cid}
    delivered, offline = [], []
    for m in members:
        if m == username:
            continue
        ws = connected_clients.get(m)
        if ws is not None:
            try:
                await ws.send_json(payload)
                delivered.append(m)
            except Exception:
                offline.append(m)      # a dead socket is not a failed send:
        else:                          # the row is already committed and the
            offline.append(m)          # recipient will read it from history
    return {"ok": True, "conversation_id": cid, "timestamp": ts,
            "delivered": delivered, "offline": offline}


# ── FLEET SYNC ──────────────────────────────────────────────────────────
# Every node keeps the whole log and asks peers for what it lacks. Routing a
# message to the node its recipient "lives on" would need a directory of who
# is where, kept correct while nodes come and go; replication needs none of
# that and is idempotent, which matters when the transport is a house with
# the power out.
#
# A message's identity is a hash of its content, so two nodes independently
# holding the same message agree it is the same message without a schema
# change, a coordinator, or an id allocator. Clock skew cannot create a
# duplicate because the timestamp is inside the hash.
#
# This endpoint is READ-ONLY and exports nothing but what the local store
# already holds. The peer does its own inserting, so there is no write path
# here for a peer to abuse.
import hashlib as _hashlib

FLEET_TOKEN_FILE = "/etc/ele-messenger/fleet.token"


def _fleet_token():
    """Shared secret for peer sync. Absent means sync is OFF, not open."""
    try:
        with open(FLEET_TOKEN_FILE) as fh:
            t = fh.read().strip()
        return t or None
    except Exception:
        return None


def message_id(conversation_id, from_user, timestamp, message, image_id):
    """Stable across nodes: same message, same id, computed anywhere.

    Field separator is \x1f (unit separator) rather than a printable
    character, so a message whose text contains the separator cannot be
    made to collide with a different message by someone choosing their
    words carefully.
    """
    parts = [conversation_id or "", from_user or "", timestamp or "",
             message or "", image_id or ""]
    return _hashlib.sha256("\x1f".join(parts).encode("utf-8")).hexdigest()[:32]


@app.get("/api/sync/export")
async def sync_export(token: str = None, days: int = 30):
    ft = _fleet_token()
    if not ft:
        raise HTTPException(503, "fleet sync not configured on this node")
    if token != ft:
        raise HTTPException(401, "invalid fleet token")
    days = max(1, min(int(days), 365))
    cutoff = (datetime.now() - timedelta(days=days)).isoformat()
    with get_db() as conn:
        rows = conn.execute(
            "SELECT conversation_id, from_user, message, image_id, timestamp, secret "
            "FROM messages WHERE timestamp > ? ORDER BY timestamp ASC",
            (cutoff,)
        ).fetchall()
        members = conn.execute(
            "SELECT conversation_id, username FROM conversation_members"
        ).fetchall()
        convos = conn.execute("SELECT id, secret FROM conversations").fetchall()
    msgs = []
    for r in rows:
        msgs.append({
            "id": message_id(r["conversation_id"], r["from_user"],
                             r["timestamp"], r["message"], r["image_id"]),
            "conversation_id": r["conversation_id"],
            "from": r["from_user"],
            "message": r["message"],
            "image_id": r["image_id"],
            "timestamp": r["timestamp"],
            "secret": bool(r["secret"]),
        })
    return {
        "node": socket.gethostname().split(".")[0],
        "messages": msgs,
        "members": [{"conversation_id": m["conversation_id"],
                     "username": m["username"]} for m in members],
        "conversations": [{"id": c["id"], "secret": bool(c["secret"])}
                          for c in convos],
    }


@app.websocket("/ws/{username}")
async def websocket_endpoint(websocket: WebSocket, username: str):
    token = websocket.query_params.get("token")
    if not token or sessions.get(token) != username:
        await websocket.close(code=4001)
        return
    await websocket.accept()
    connected_clients[username] = websocket
    connect_log.append({"time": datetime.now().isoformat(), "event": f"{username} connected"})
    try:
        while True:
            data = await websocket.receive_json()
            if data.get("type") == "ping":
                await websocket.send_json({"type": "pong"})
                continue
            target = data.get("to")
            message = data.get("message") or ""
            image_id = data.get("image_id")
            secret = 1 if data.get("secret") else 0
            if target and (message or image_id):
                cid = convo_id(username, target)
                ts = datetime.now().isoformat()
                with get_db() as conn:
                    # Ensure conversation and members exist
                    conn.execute(
                        "INSERT OR IGNORE INTO conversations (id, secret) VALUES (?, ?)",
                        (cid, secret)
                    )
                    conn.execute(
                        "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                        (cid, username)
                    )
                    conn.execute(
                        "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                        (cid, target)
                    )
                    conn.execute(
                        "INSERT INTO messages (conversation_id, from_user, message, image_id, timestamp, secret) VALUES (?, ?, ?, ?, ?, ?)",
                        (cid, username, message, image_id, ts, secret)
                    )
                    conn.commit()
            if target in connected_clients:
                await connected_clients[target].send_json({
                    "from": username,
                    "message": message,
                    "image_id": image_id,
                    "secret": bool(secret)
                })
            else:
                await websocket.send_json({"from": "server", "message": f"{target} is not online."})
    except WebSocketDisconnect:
        del connected_clients[username]
        connect_log.append({"time": datetime.now().isoformat(), "event": f"{username} disconnected"})
