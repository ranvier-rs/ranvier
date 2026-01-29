# Ranvier Full-Stack Demo (Separated Mode)

This demo showcases how to integrate a **SvelteKit Frontend** with a **Ranvier Backend**.

## Architecture

- **Backend (Port 3030)**: Runs the Ranvier Workflow Engine.
    - Uses `tiny_http` to listen for requests.
    - Implements a `WaitForRequestNode` using the `HttpListenerSynapse`.
    - Responds immediately with CORS headers allowing cross-origin calls.
- **Frontend (Port 5173)**: A standard SvelteKit application.
    - Sends `POST` requests to the Ranvier Backend.

## How to Run

You need two terminal windows.

### Terminal 1: Backend
```bash
cargo run -p fullstack-demo
```
*It will print: `[HttpListener] Listening on 127.0.0.1:3030`*

### Terminal 2: Frontend
```bash
cd ranvier/examples/fullstack-demo/frontend
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
    [Node] Waiting for HTTP Order...
    [HttpListener] Waiting for request...
    [Node] Received POST /api/order
    [Node] Processing Order from URL: /api/order
    [Node] Finished: ORDER-SUCCESS-999
    ```
