# Ranvier Full-Stack Demo (Separated Mode)

This demo showcases how to integrate a **SvelteKit Frontend** with a **Ranvier Backend**.
This project is under `examples/experimental` and is not a canonical architecture reference.

## Architecture

- **Backend (Port 3030)**: Runs the Ranvier Workflow Engine.
    - Uses `tiny_http` to listen for requests.
    - Implements a simple `ProcessDataNode` using `HttpListenerSynapse`.
    - Responds immediately with CORS headers allowing cross-origin calls.
- **Frontend (Port 5173)**: A standard SvelteKit application.
    - Sends `POST /api/order` requests to the backend.

## How to Run

You need two terminal windows.

### Terminal 1: Backend
```bash
cargo run -p fullstack-demo
```
*It will print: `[HttpListener] Listening on 127.0.0.1:3030`*

### Terminal 2: Frontend
```bash
cd ranvier/examples/experimental/fullstack-demo/frontend
# If you haven't installed dependencies yet
# npm install
npm run dev
```
*Open `http://localhost:5173` in your browser.*

## Usage
1.  Click the **"Place Order"** button on the web page.
2.  Observe the Frontend status change to "Success!".
3.  Observe the Backend logs showing:
    ```text
    [HttpListener] Waiting for request...
    [Node] Received POST /api/order
    ```
