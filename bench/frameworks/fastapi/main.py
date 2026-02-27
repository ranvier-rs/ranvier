from fastapi import FastAPI, Depends, HTTPException, Header
from fastapi.responses import JSONResponse
import asyncio
import time
import jwt

# This file defines 4 separate FastAPI instances so we can run them on ports 3000-3003
app1 = FastAPI()
app2 = FastAPI()
app3 = FastAPI()
app4 = FastAPI()

# -------------------------------------------------------------
# Scenario 1: Simple CRUD
# Matches axum_scenario1.rs and scenario1_server.rs
# -------------------------------------------------------------
@app1.get("/")
async def root():
    return JSONResponse(status_code=200, content={"message": "Hello, World!", "status": 200})

# -------------------------------------------------------------
# Scenario 2: Complex Auth Flow
# Matches axum_scenario2.rs and scenario2_server.rs
# -------------------------------------------------------------
SECRET_KEY = "bench-secret-key"

async def verify_token(authorization: str = Header(None)):
    if not authorization or not authorization.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="Unauthorized in Bus")
    
    token = authorization.split(" ")[1]
    try:
        payload = jwt.decode(token, SECRET_KEY, algorithms=["HS256"])
        if "admin" not in payload.get("roles", []):
            raise HTTPException(status_code=403, detail="Forbidden")
        return payload
    except Exception:
        raise HTTPException(status_code=401, detail="Unauthorized in Bus")

@app2.get("/protected")
async def protected(payload: dict = Depends(verify_token)):
    return JSONResponse(content={
        "subject": payload.get("sub"),
        "message": "Access Granted via Typed Bus Capability"
    })

# -------------------------------------------------------------
# Scenario 3: Multi-step Workflow
# Matches axum_scenario3.rs and scenario3_server.rs
# -------------------------------------------------------------
@app3.get("/workflow")
async def workflow():
    counter = 1
    history = ["step1"]
    
    counter *= 10
    history.append("step2")
    
    counter += 5
    history.append("step3")
    
    return JSONResponse(content={
        "final_counter": counter,
        "history": history,
        "status": "workflow-complete"
    })

# -------------------------------------------------------------
# Scenario 4: High Concurrency
# Matches axum_scenario4.rs and scenario4_server.rs
# -------------------------------------------------------------
@app4.get("/concurrency")
async def concurrency():
    await asyncio.sleep(0.005)
    return JSONResponse(content={
        "status": "success",
        "processed_at": int(time.time() * 1000)
    })
