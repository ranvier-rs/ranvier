const express = require('express');
const jwt = require('jsonwebtoken');

const SECRET_KEY = "bench-secret-key";

// Helper to start an app on a specific port
function startApp(app, port, name) {
  app.listen(port, "0.0.0.0", () => {
    console.log(`Starting Express Benchmark Server (Scenario ${name}) on 0.0.0.0:${port}`);
  });
}

// -------------------------------------------------------------
// Scenario 1: Simple CRUD (Port 3000)
// Matches axum_scenario1.rs and scenario1_server.rs
// -------------------------------------------------------------
const app1 = express();
app1.get('/', (req, res) => {
  res.status(200).json({ message: "Hello, World!", status: 200 });
});
if (process.env.SCENARIO === "1" || !process.env.SCENARIO) {
  startApp(app1, 3000, "1: Simple CRUD");
}

// -------------------------------------------------------------
// Scenario 2: Complex Auth Flow (Port 3001)
// Matches axum_scenario2.rs and scenario2_server.rs
// -------------------------------------------------------------
const app2 = express();

function verifyToken(req, res, next) {
  const authHeader = req.headers.authorization;
  if (!authHeader || !authHeader.startsWith("Bearer ")) {
    return res.status(401).json({ detail: "Unauthorized in Bus" });
  }
  const token = authHeader.split(" ")[1];
  try {
    const payload = jwt.verify(token, SECRET_KEY, { algorithms: ["HS256"] });
    if (!payload.roles || !payload.roles.includes("admin")) {
      return res.status(403).json({ detail: "Forbidden" });
    }
    req.user = payload;
    next();
  } catch (err) {
    return res.status(401).json({ detail: "Unauthorized in Bus" });
  }
}

app2.get('/protected', verifyToken, (req, res) => {
  res.json({
    subject: req.user.sub,
    message: "Access Granted via Typed Bus Capability"
  });
});

if (process.env.SCENARIO === "2" || !process.env.SCENARIO) {
  startApp(app2, 3001, "2: Complex Auth");
}

// -------------------------------------------------------------
// Scenario 3: Multi-step Workflow (Port 3002)
// Matches axum_scenario3.rs and scenario3_server.rs
// -------------------------------------------------------------
const app3 = express();
app3.get('/workflow', (req, res) => {
  let counter = 1;
  let history = ["step1"];

  counter *= 10;
  history.push("step2");

  counter += 5;
  history.push("step3");

  res.json({
    final_counter: counter,
    history: history,
    status: "workflow-complete"
  });
});

if (process.env.SCENARIO === "3" || !process.env.SCENARIO) {
  startApp(app3, 3002, "3: Multi-step Workflow");
}

// -------------------------------------------------------------
// Scenario 4: High Concurrency (Port 3003)
// Matches axum_scenario4.rs and scenario4_server.rs
// -------------------------------------------------------------
const app4 = express();
app4.get('/concurrency', async (req, res) => {
  // wait 5 milliseconds
  await new Promise(resolve => setTimeout(resolve, 5));
  res.json({
    status: "success",
    processed_at: Date.now()
  });
});

if (process.env.SCENARIO === "4" || !process.env.SCENARIO) {
  startApp(app4, 3003, "4: High Concurrency");
}
