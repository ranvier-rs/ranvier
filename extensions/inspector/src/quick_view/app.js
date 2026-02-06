async function getJson(path) {
  const res = await fetch(path);
  if (!res.ok) {
    throw new Error(`${path} ${res.status}`);
  }
  return res.json();
}

function el(name, attrs = {}) {
  const node = document.createElementNS("http://www.w3.org/2000/svg", name);
  for (const [k, v] of Object.entries(attrs)) {
    node.setAttribute(k, String(v));
  }
  return node;
}

function drawGraph(svg, schematic, internalTrace) {
  svg.replaceChildren();
  const nodes = schematic?.nodes ?? [];
  const edges = schematic?.edges ?? [];
  const traceNodes = new Set((internalTrace?.nodes ?? []).map((n) => n.node_id));
  const faultNodes = new Set(
    (internalTrace?.nodes ?? [])
      .filter((n) => (n.outcome_type || "").toLowerCase() === "fault")
      .map((n) => n.node_id),
  );

  const width = 160;
  const height = 46;
  const gap = 24;
  const x0 = 28;
  const y = 130;
  const indexOf = new Map();
  nodes.forEach((n, i) => indexOf.set(n.id, i));

  for (const edge of edges) {
    const from = indexOf.get(edge.from);
    const to = indexOf.get(edge.to);
    if (from == null || to == null) continue;
    const x1 = x0 + from * (width + gap) + width;
    const x2 = x0 + to * (width + gap);
    const line = el("line", {
      x1,
      y1: y + height / 2,
      x2,
      y2: y + height / 2,
      class: traceNodes.has(edge.from) && traceNodes.has(edge.to) ? "edge active" : "edge",
    });
    svg.appendChild(line);
  }

  for (let i = 0; i < nodes.length; i += 1) {
    const n = nodes[i];
    const x = x0 + i * (width + gap);
    const rect = el("rect", {
      x,
      y,
      width,
      height,
      rx: 8,
      class: `node${traceNodes.has(n.id) ? " active" : ""}${faultNodes.has(n.id) ? " fault" : ""}`,
    });
    const label = el("text", {
      x: x + 10,
      y: y + 18,
      class: "label",
    });
    label.textContent = n.label || n.id;
    const kind = el("text", {
      x: x + 10,
      y: y + 34,
      class: "label",
      opacity: "0.7",
    });
    kind.textContent = n.kind || "";
    svg.append(rect, label, kind);
  }
}

function renderTrace(trace) {
  const list = document.getElementById("trace-list");
  list.replaceChildren();
  const nodes = trace?.nodes ?? [];
  for (const n of nodes) {
    const li = document.createElement("li");
    li.textContent = `${n.node_id} -> ${n.outcome_type}${n.branch_id ? ` (${n.branch_id})` : ""}`;
    list.appendChild(li);
  }
}

function renderPublic(pub) {
  const out = document.getElementById("public-view");
  out.textContent = JSON.stringify(pub, null, 2);
}

function renderMeta(schematic) {
  const meta = document.getElementById("circuit-meta");
  const n = schematic?.nodes?.length ?? 0;
  const e = schematic?.edges?.length ?? 0;
  meta.textContent = `${schematic?.name ?? "unknown"} | nodes=${n}, edges=${e}`;
}

async function reload() {
  try {
    const [schematic, traceInternal, tracePublic] = await Promise.all([
      getJson("/schematic"),
      getJson("/trace/internal"),
      getJson("/trace/public"),
    ]);
    renderMeta(schematic);
    renderTrace(traceInternal);
    renderPublic(tracePublic);
    drawGraph(document.getElementById("graph"), schematic, traceInternal);
  } catch (err) {
    document.getElementById("circuit-meta").textContent = `Load failed: ${err.message}`;
  }
}

document.getElementById("reload-btn").addEventListener("click", reload);
reload();
