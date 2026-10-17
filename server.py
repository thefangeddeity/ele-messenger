# ELE Messenger - server.py v1.4.1
import json, os, secrets, asyncio, uuid, sqlite3, base64
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request
from fastapi.responses import Response
import socket
from datetime import datetime, timedelta, timezone
from contextlib import contextmanager
import bcrypt
import time

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

@contextmanager
def get_db():
    """A connection for one `with` block: committed on success, rolled back
    on error, and always closed.

    sqlite3's own `with conn:` only commits or rolls back; it never closes.
    Every request used to leave its connection open until garbage collection
    happened to reach it, and a long-running server ran out of file handles:
    1,010 open copies of ele.db, then "unable to open database file" on every
    request, which the web client showed as a login form."""
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    try:
        with conn:
            yield conn
    finally:
        conn.close()

def hash_pin(pin: str) -> str:
    return bcrypt.hashpw(pin.encode(), bcrypt.gensalt()).decode()

def is_hashed(stored: str) -> bool:
    return stored.startswith("$2")

def verify_pin(pin: str, stored: str) -> bool:
    if not is_hashed(stored):
        # Accounts created before hashing still hold the plaintext PIN.
        return secrets.compare_digest(pin, stored)
    try:
        return bcrypt.checkpw(pin.encode(), stored.encode())
    except ValueError:
        return False

# Rate limit tracking: {username: (fail_count, last_attempt_time)}
login_attempts = {}

def check_rate_limit(username: str) -> float:
    if username not in login_attempts:
        return 0
    fail_count, last_time = login_attempts[username]
    time_since = time.time() - last_time
    backoff = 2 ** min(fail_count, 5)  # Max 32s backoff after 5 failures
    remaining = max(0, backoff - time_since)
    return remaining

def record_failed_login(username: str):
    if username not in login_attempts:
        login_attempts[username] = (1, time.time())
    else:
        count, _ = login_attempts[username]
        login_attempts[username] = (count + 1, time.time())

def clear_login_attempts(username: str):
    login_attempts.pop(username, None)

def convo_id(user_a, user_b, secret=False):
    """Deterministic conversation ID for a pair of users."""
    prefix = "secret:" if secret else ""
    return prefix + ":".join(sorted([user_a, user_b]))

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

        # Add name column to conversations if missing (group chat)
        convo_cols = [r[1] for r in conn.execute("PRAGMA table_info(conversations)").fetchall()]
        if "name" not in convo_cols:
            conn.execute("ALTER TABLE conversations ADD COLUMN name TEXT")

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

        # Which node a message was authored on. NULL means "this one" -- the
        # exporter resolves NULL to its own name on the way out, so a message
        # authored through any code path (REST or websocket) gets a correct
        # origin without every write site having to remember to set it.
        msg_cols = [r[1] for r in conn.execute("PRAGMA table_info(messages)").fetchall()]
        if "origin_node" not in msg_cols:
            conn.execute("ALTER TABLE messages ADD COLUMN origin_node TEXT")

        # Default admin account
        count = conn.execute("SELECT COUNT(*) FROM accounts").fetchone()[0]
        if count == 0:
            hashed_pin = hash_pin("1234")
            conn.execute(
                "INSERT INTO accounts (username, pin) VALUES (?, ?)",
                ("admin", hashed_pin)
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
        cutoff = (datetime.now(timezone.utc) - timedelta(days=7)).isoformat()
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
        hashed_pin = hash_pin(pin)
        conn.execute("INSERT INTO accounts (username, pin) VALUES (?, ?)", (username, hashed_pin))
        conn.execute("DELETE FROM accounts WHERE username = ?", ("admin",))
        conn.commit()
    return {"status": "ok"}

@app.post("/api/login")
async def login(data: dict):
    username = data.get("username", "").strip()
    pin = data.get("pin", "").strip()

    # Rate limiting
    wait_time = check_rate_limit(username)
    if wait_time > 0:
        raise HTTPException(429, f"too many failed attempts, try again in {int(wait_time)+1}s")

    with get_db() as conn:
        row = conn.execute("SELECT pin FROM accounts WHERE username = ?", (username,)).fetchone()

    if not row or not verify_pin(pin, row["pin"]):
        record_failed_login(username)
        raise HTTPException(401, "invalid credentials")

    if not is_hashed(row["pin"]):
        with get_db() as conn:
            conn.execute("UPDATE accounts SET pin = ? WHERE username = ?", (hash_pin(pin), username))
            conn.commit()

    clear_login_attempts(username)
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
    cutoff = (datetime.now(timezone.utc) - timedelta(days=7)).isoformat()
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
                   m.origin_node, c.secret as convo_secret
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
            "secret": bool(r["secret"] or r["convo_secret"]),
            "origin_node": r["origin_node"] or _this_node(),
        })
    return {"history": history}

@app.get("/api/secret/{other_user}")
async def get_secret(other_user: str, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user, secret=True)
    with get_db() as conn:
        row = conn.execute("SELECT id FROM conversations WHERE id = ?", (cid,)).fetchone()
    return {"secret": bool(row) if row else False}

@app.post("/api/secret/{other_user}")
async def set_secret(other_user: str, data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user, secret=True)
    with get_db() as conn:
        conn.execute(
            "INSERT OR IGNORE INTO conversations (id, secret) VALUES (?, 1)",
            (cid,)
        )
        conn.execute("INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)", (cid, username))
        conn.execute("INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)", (cid, other_user))
        conn.commit()
    payload = {"type": "secret_chat_started", "with": other_user}
    if username in connected_clients:
        await connected_clients[username].send_json(payload)
    payload2 = {"type": "secret_chat_started", "with": username}
    if other_user in connected_clients:
        await connected_clients[other_user].send_json(payload2)
    return {"status": "ok", "secret": True}

@app.delete("/api/history/{other_user}")
async def delete_history(other_user: str, token: str = None, secret: bool = False):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    cid = convo_id(username, other_user, secret=secret)
    with get_db() as conn:
        conn.execute("DELETE FROM messages WHERE conversation_id = ?", (cid,))
        conn.execute("DELETE FROM conversation_members WHERE conversation_id = ?", (cid,))
        conn.execute("DELETE FROM conversations WHERE id = ?", (cid,))
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
# ── THE HOUSE CHANNEL ───────────────────────────────────────────────────
# One rolling conversation everyone is in, rather than a grid of pairs.
#
# Person-to-person threads assume you know who to tell. In the event this is
# built for, you do not: you have something to say about the house and it
# should reach whoever is listening. Picking a recipient from a dropdown is a
# decision nobody should have to make while reporting that the stairs are
# blocked.
#
# Membership is implicit. Anyone who can authenticate against this node can
# speak into the channel and is added on first use, so there is no invite
# step and no way to be left out of the one conversation that matters.
HOUSE = "house"


def _join_house(conn, username):
    conn.execute("INSERT OR IGNORE INTO conversations (id, secret) VALUES (?, 0)",
                 (HOUSE,))
    conn.execute(
        "INSERT OR IGNORE INTO conversation_members (conversation_id, username) "
        "VALUES (?, ?)", (HOUSE, username))


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
    ts = datetime.now(timezone.utc).isoformat()

    with get_db() as conn:
        if cid:
            if cid == HOUSE:
                _join_house(conn, username)
            row = conn.execute("SELECT id FROM conversations WHERE id = ?", (cid,)).fetchone()
            if not row:
                raise HTTPException(404, "conversation not found")
            member_rows = conn.execute(
                "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid,)
            ).fetchall()
            members = [m["username"] for m in member_rows]
            if cid == HOUSE and username not in members:
                # Open channel: speaking into it is how you join it.
                _join_house(conn, username)
                members = members + [username]
            elif username not in members:
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
            "INSERT INTO messages (conversation_id, from_user, message, image_id, "
            "timestamp, secret, origin_node) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (cid, username, message, image_id, ts, secret, _this_node()))
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


def _this_node():
    return socket.gethostname().split(".")[0]


def _fleet_token():
    """Shared secret for peer sync. Absent means sync is OFF, not open."""
    try:
        with open(FLEET_TOKEN_FILE) as fh:
            t = fh.read().strip()
        return t or None
    except Exception:
        return None


def canonical_ms(ts):
    """A timestamp as epoch milliseconds, for hashing.

    Identity must not depend on how precisely or in what format a timestamp
    was written down. This store keeps microseconds; the mesh keeps
    milliseconds; this fleet has emitted '+00:00', 'Z' and bare naive stamps
    for the same instant. Hashed as written, one message became several --
    measured as the same message stored twice the moment it could arrive by
    two paths.

    Milliseconds because that is the coarsest precision any transport here
    promises, and identity is only as fine as the least precise carrier.

    Must stay identical to sync-peers' canonical_ms and to the message
    buffer's: the three compute the id of the same message and have to agree.
    """
    if not ts:
        return None
    try:
        d = datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
        if d.tzinfo is None:
            d = d.replace(tzinfo=timezone.utc)
        # Integer arithmetic, not timestamp() * 1000: an epoch in milliseconds
        # needs sixteen significant digits, which is the edge of a float64, so
        # the multiply can round -- and an id that is sometimes a millisecond
        # off is an id that sometimes duplicates a message.
        return int(d.timestamp()) * 1000 + d.microsecond // 1000
    except Exception:
        return None


def message_id(conversation_id, from_user, timestamp, message, image_id):
    """Stable across nodes AND across transports: same message, same id,
    computed anywhere, however it travelled to get here.

    Field separator is \x1f (unit separator) rather than a printable
    character, so a message whose text contains the separator cannot be
    made to collide with a different message by someone choosing their
    words carefully.
    """
    ms = canonical_ms(timestamp)
    when = str(ms) if ms is not None else (timestamp or "")
    parts = [conversation_id or "", from_user or "", when,
             message or "", image_id or ""]
    return _hashlib.sha256("\x1f".join(parts).encode("utf-8")).hexdigest()[:32]


# ── THE NODE IS A CHAT USER ─────────────────────────────────────────────
# A viewer opened on this node is already somebody: this machine. It does not
# have to be told who it is, and in an outage nobody should be typing a PIN
# into a wall display to report that the stairs are blocked.
#
# So the node holds an account named after itself and hands out a session for
# it to callers on the local network. Logging in as a PERSON stays available
# and is strictly an addition -- it says who is writing, which the words can
# also say. The node identity says which machine is still talking, which the
# words cannot.
#
# Restricted to private address space. That is a real boundary on a household
# LAN and an honest one to state: anyone already inside the network can speak
# as this node, exactly as anyone standing in the house could speak into it.
_PRIVATE_PREFIXES = ("127.", "10.", "192.168.", "169.254.")


def _is_local_caller(request):
    host = (request.client.host if request and request.client else "") or ""
    if host in ("::1", "localhost"):
        return True
    if host.startswith(_PRIVATE_PREFIXES):
        return True
    if host.startswith("172."):                      # 172.16.0.0/12 only
        try:
            return 16 <= int(host.split(".")[1]) <= 31
        except Exception:
            return False
    if host.startswith("100."):                      # tailnet CGNAT 100.64/10
        try:
            return 64 <= int(host.split(".")[1]) <= 127
        except Exception:
            return False
    return False


@app.post("/api/node-login")
async def node_login(request: Request = None):
    if not _is_local_caller(request):
        raise HTTPException(403, "node sessions are for the local network only")
    node = _this_node()
    with get_db() as conn:
        row = conn.execute("SELECT username FROM accounts WHERE username = ?",
                           (node,)).fetchone()
        if not row:
            # The PIN is never shown and never typed; it exists because the
            # accounts table requires one. The node's identity is the node,
            # not a secret someone has to remember.
            # hash_pin, not the raw value. Nobody ever types this PIN -- the
            # node's identity is the node, and the column simply has to hold
            # something -- but a plaintext secret here would sit in the table
            # forever: the legacy-plaintext path only rehashes on a LOGIN,
            # and no one ever logs in as a machine.
            conn.execute("INSERT INTO accounts (username, pin) VALUES (?, ?)",
                         (node, hash_pin(secrets.token_hex(16))))
            conn.commit()
    token = secrets.token_hex(16)
    sessions[token] = node
    return {"token": token, "username": node, "kind": "node"}


@app.get("/api/sync/export")
async def sync_export(token: str = None, days: int = 30):
    ft = _fleet_token()
    if not ft:
        raise HTTPException(503, "fleet sync not configured on this node")
    if token != ft:
        raise HTTPException(401, "invalid fleet token")
    days = max(1, min(int(days), 365))
    cutoff = (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()
    with get_db() as conn:
        rows = conn.execute(
            "SELECT conversation_id, from_user, message, image_id, timestamp, secret, "
            "origin_node FROM messages WHERE timestamp > ? ORDER BY timestamp ASC",
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
            # NULL means it was authored right here, so this node's name is
            # the correct answer -- which is why no write path needs to set it.
            "origin_node": r["origin_node"] or _this_node(),
        })
    return {
        "node": socket.gethostname().split(".")[0],
        "messages": msgs,
        "members": [{"conversation_id": m["conversation_id"],
                     "username": m["username"]} for m in members],
        "conversations": [{"id": c["id"], "secret": bool(c["secret"])}
                          for c in convos],
    }

@app.get("/api/conversations")
async def get_conversations(token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    with get_db() as conn:
        convo_rows = conn.execute(
            "SELECT conversation_id FROM conversation_members WHERE username = ?",
            (username,)
        ).fetchall()
        convo_ids = [r["conversation_id"] for r in convo_rows]
        result = []
        for cid in convo_ids:
            meta = conn.execute("SELECT id, secret, name FROM conversations WHERE id = ?", (cid,)).fetchone()
            if not meta:
                continue
            members = conn.execute(
                "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid,)
            ).fetchall()
            member_list = [m["username"] for m in members]
            result.append({
                "conversation_id": cid,
                "name": meta["name"] or ", ".join(member_list),
                "members": member_list,
                "secret": bool(meta["secret"])
            })
    return {"conversations": result}

@app.post("/api/conversation")
async def create_conversation(data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    members = data.get("members", [])
    name = data.get("name", "").strip() or None
    if not isinstance(members, list) or len(members) < 1:
        raise HTTPException(400, "members list required")
    all_members = list(dict.fromkeys([username] + members))  # dedupe, preserve order
    cid = str(uuid.uuid4())
    auto_name = name or ", ".join(all_members)
    with get_db() as conn:
        conn.execute(
            "INSERT INTO conversations (id, secret, name) VALUES (?, 0, ?)",
            (cid, auto_name if not name else name)
        )
        for m in all_members:
            conn.execute(
                "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
                (cid, m)
            )
        conn.commit()
    for m in all_members:
        if m in connected_clients:
            await connected_clients[m].send_json({
                "type": "conversation_created",
                "conversation_id": cid,
                "name": auto_name if not name else name,
                "members": all_members,
                "created_by": username
            })
    return {"conversation_id": cid, "name": auto_name if not name else name, "members": all_members}

@app.post("/api/conversation/{cid}/members")
async def add_member(cid: str, data: dict, token: str = None):
    if not token or token not in sessions:
        raise HTTPException(401, "invalid token")
    username = sessions[token]
    new_member = data.get("username", "").strip()
    if not new_member:
        raise HTTPException(400, "username required")
    with get_db() as conn:
        row = conn.execute(
            "SELECT id, name FROM conversations WHERE id = ?", (cid,)
        ).fetchone()
        if not row:
            raise HTTPException(404, "conversation not found")
        caller = conn.execute(
            "SELECT username FROM conversation_members WHERE conversation_id = ? AND username = ?",
            (cid, username)
        ).fetchone()
        if not caller:
            raise HTTPException(403, "not a member")
        conn.execute(
            "INSERT OR IGNORE INTO conversation_members (conversation_id, username) VALUES (?, ?)",
            (cid, new_member)
        )
        conn.commit()
        members = conn.execute(
            "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid,)
        ).fetchall()
        member_list = [m["username"] for m in members]
    notify = {"type": "member_added", "conversation_id": cid, "username": new_member, "members": member_list}
    for m in member_list:
        if m in connected_clients:
            await connected_clients[m].send_json(notify)
    return {"status": "ok", "members": member_list}

@app.websocket("/ws/{username}")
async def websocket_endpoint(websocket: WebSocket, username: str):
    token = websocket.query_params.get("token")
    if not token or sessions.get(token) != username:
        await websocket.close(code=4001)
        return
    await websocket.accept()
    connected_clients[username] = websocket
    connect_log.append({"time": datetime.now(timezone.utc).isoformat(), "event": f"{username} connected"})
    try:
        while True:
            data = await websocket.receive_json()
            if data.get("type") == "ping":
                await websocket.send_json({"type": "pong"})
                continue
            target = data.get("to")
            cid_direct = data.get("conversation_id")
            message = data.get("message") or ""
            image_id = data.get("image_id")
            secret = 1 if data.get("secret") else 0
            if cid_direct and (message or image_id):
                # Group (or explicit cid) message — fan out to all members
                ts = datetime.now(timezone.utc).isoformat()
                with get_db() as conn:
                    row = conn.execute("SELECT id FROM conversations WHERE id = ?", (cid_direct,)).fetchone()
                    if not row:
                        await websocket.send_json({"from": "server", "message": "conversation not found"})
                        continue
                    member_rows = conn.execute(
                        "SELECT username FROM conversation_members WHERE conversation_id = ?", (cid_direct,)
                    ).fetchall()
                    member_list = [m["username"] for m in member_rows]
                    conn.execute(
                        "INSERT INTO messages (conversation_id, from_user, message, image_id, timestamp, secret) VALUES (?, ?, ?, ?, ?, ?)",
                        (cid_direct, username, message, image_id, ts, secret)
                    )
                    conn.commit()
                payload = {"from": username, "message": message, "image_id": image_id, "secret": bool(secret), "conversation_id": cid_direct}
                offline = []
                for m in member_list:
                    if m == username:
                        continue
                    if m in connected_clients:
                        await connected_clients[m].send_json(payload)
                    else:
                        offline.append(m)
                if offline:
                    await websocket.send_json({"from": "server", "message": f"{", ".join(offline)} not online"})
            elif target and (message or image_id):
                # 1:1 message — legacy path
                cid = convo_id(username, target, secret=bool(secret))
                ts = datetime.now(timezone.utc).isoformat()
                with get_db() as conn:
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
                        "secret": bool(secret),
                        "conversation_id": cid
                    })
                else:
                    await websocket.send_json({"from": "server", "message": f"{target} is not online."})
    except WebSocketDisconnect:
        del connected_clients[username]
        connect_log.append({"time": datetime.now(timezone.utc).isoformat(), "event": f"{username} disconnected"})
