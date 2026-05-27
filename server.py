# ELE Messenger - server.py v0.3.0
import json, os, secrets
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from datetime import datetime

CONFIG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "config.json")

def load_config():
    try:
        with open(CONFIG_PATH) as f:
            return json.load(f)
    except Exception:
        return {}

app = FastAPI()
connected_clients: dict[str, WebSocket] = {}
connect_log: list[dict] = []

accounts: dict = {
    "Ron": {"pin": "5454"},
    "Carm": {"pin": "7833"},
}
sessions: dict = {}

@app.post("/api/register")
async def register(data: dict):
    username = data.get("username", "").strip()
    pin = data.get("pin", "").strip()
    if not username or not pin:
        raise HTTPException(400, "username and pin required")
    if len(pin) != 4 or not pin.isdigit():
        raise HTTPException(400, "pin must be 4 digits")
    if username in accounts:
        raise HTTPException(409, "username taken")
    accounts[username] = {"pin": pin}
    return {"status": "ok"}

@app.post("/api/login")
async def login(data: dict):
    username = data.get("username", "").strip()
    pin = data.get("pin", "").strip()
    if username not in accounts:
        raise HTTPException(401, "invalid credentials")
    if accounts[username]["pin"] != pin:
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

@app.get("/online")
async def online_users():
    return {"online": list(connected_clients.keys())}

@app.get("/api/log")
async def get_log():
    return {"log": connect_log[-50:]}

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
            target = data.get("to")
            message = data.get("message")
            if target in connected_clients:
                await connected_clients[target].send_json({"from": username, "message": message})
            else:
                await websocket.send_json({"from": "server", "message": f"{target} is not online."})
    except WebSocketDisconnect:
        del connected_clients[username]
        connect_log.append({"time": datetime.now().isoformat(), "event": f"{username} disconnected"})
