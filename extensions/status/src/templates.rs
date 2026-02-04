//! HTML 템플릿
//!
//! 정적 Status Page 생성에 필요한 CSS/JS/HTML 템플릿

/// 기본 CSS 스타일
pub const DEFAULT_CSS: &str = r#"
:root {
  --bg-primary: #0f0f0f;
  --bg-secondary: #1a1a1a;
  --bg-tertiary: #252525;
  --text-primary: #ffffff;
  --text-secondary: #a0a0a0;
  --text-muted: #666666;
  --border-color: #333333;
  --accent-green: #22c55e;
  --accent-yellow: #eab308;
  --accent-orange: #f97316;
  --accent-red: #ef4444;
  --accent-blue: #3b82f6;
}

@media (prefers-color-scheme: light) {
  :root {
    --bg-primary: #ffffff;
    --bg-secondary: #f8f9fa;
    --bg-tertiary: #e9ecef;
    --text-primary: #1a1a1a;
    --text-secondary: #495057;
    --text-muted: #868e96;
    --border-color: #dee2e6;
  }
}

* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
  background-color: var(--bg-primary);
  color: var(--text-primary);
  line-height: 1.6;
  min-height: 100vh;
}

.container {
  max-width: 720px;
  margin: 0 auto;
  padding: 2rem 1.5rem;
}

header {
  text-align: center;
  margin-bottom: 3rem;
}

.logo {
  font-size: 1.5rem;
  font-weight: 700;
  margin-bottom: 0.5rem;
}

.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.75rem 1.5rem;
  border-radius: 9999px;
  font-weight: 600;
  font-size: 1rem;
  margin: 1rem 0;
}

.status-operational { background-color: rgba(34, 197, 94, 0.15); color: var(--accent-green); }
.status-degraded { background-color: rgba(234, 179, 8, 0.15); color: var(--accent-yellow); }
.status-partial { background-color: rgba(249, 115, 22, 0.15); color: var(--accent-orange); }
.status-major { background-color: rgba(239, 68, 68, 0.15); color: var(--accent-red); }
.status-maintenance { background-color: rgba(59, 130, 246, 0.15); color: var(--accent-blue); }

.last-updated {
  color: var(--text-muted);
  font-size: 0.875rem;
}

.section {
  margin-bottom: 2.5rem;
}

.section-title {
  font-size: 0.75rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--text-muted);
  margin-bottom: 1rem;
}

.circuit-list {
  background-color: var(--bg-secondary);
  border-radius: 12px;
  overflow: hidden;
  border: 1px solid var(--border-color);
}

.circuit-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 1rem 1.25rem;
  border-bottom: 1px solid var(--border-color);
}

.circuit-item:last-child {
  border-bottom: none;
}

.circuit-name {
  font-weight: 500;
}

.circuit-status {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.875rem;
}

.status-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.status-dot.operational { background-color: var(--accent-green); }
.status-dot.degraded { background-color: var(--accent-yellow); }
.status-dot.partial { background-color: var(--accent-orange); }
.status-dot.major { background-color: var(--accent-red); }
.status-dot.maintenance { background-color: var(--accent-blue); }

.incident-list {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.incident-card {
  background-color: var(--bg-secondary);
  border-radius: 12px;
  padding: 1.25rem;
  border: 1px solid var(--border-color);
}

.incident-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 0.75rem;
}

.incident-title {
  font-weight: 600;
  font-size: 1rem;
}

.incident-status {
  font-size: 0.75rem;
  font-weight: 500;
  padding: 0.25rem 0.5rem;
  border-radius: 4px;
  background-color: var(--bg-tertiary);
  color: var(--text-secondary);
}

.incident-meta {
  font-size: 0.875rem;
  color: var(--text-muted);
  margin-bottom: 1rem;
}

.incident-timeline {
  border-left: 2px solid var(--border-color);
  padding-left: 1rem;
  margin-left: 0.5rem;
}

.timeline-item {
  position: relative;
  padding-bottom: 1rem;
}

.timeline-item:last-child {
  padding-bottom: 0;
}

.timeline-item::before {
  content: '';
  position: absolute;
  left: -1.35rem;
  top: 0.35rem;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background-color: var(--border-color);
}

.timeline-time {
  font-size: 0.75rem;
  color: var(--text-muted);
  margin-bottom: 0.25rem;
}

.timeline-message {
  font-size: 0.875rem;
  color: var(--text-secondary);
}

.no-incidents {
  text-align: center;
  padding: 2rem;
  color: var(--text-muted);
  background-color: var(--bg-secondary);
  border-radius: 12px;
  border: 1px solid var(--border-color);
}

footer {
  text-align: center;
  padding: 2rem 0;
  color: var(--text-muted);
  font-size: 0.75rem;
  border-top: 1px solid var(--border-color);
  margin-top: 2rem;
}

footer a {
  color: var(--text-secondary);
  text-decoration: none;
}

footer a:hover {
  text-decoration: underline;
}
"#;

/// 기본 JavaScript
pub const DEFAULT_JS: &str = r#"
// Ranvier Status Page - Auto-refresh disabled for static page
document.addEventListener('DOMContentLoaded', function() {
  // Format relative time for timestamps
  const timeElements = document.querySelectorAll('[data-time]');
  timeElements.forEach(el => {
    const timestamp = el.getAttribute('data-time');
    if (timestamp) {
      const date = new Date(timestamp);
      el.textContent = formatRelativeTime(date);
      el.title = date.toLocaleString();
    }
  });
});

function formatRelativeTime(date) {
  const now = new Date();
  const diffMs = now - date;
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);
  const diffDays = Math.floor(diffMs / 86400000);

  if (diffMins < 1) return 'just now';
  if (diffMins < 60) return diffMins + ' min ago';
  if (diffHours < 24) return diffHours + ' hour' + (diffHours > 1 ? 's' : '') + ' ago';
  if (diffDays < 7) return diffDays + ' day' + (diffDays > 1 ? 's' : '') + ' ago';
  return date.toLocaleDateString();
}
"#;

/// HTML 템플릿 생성
pub fn generate_html(
    service_name: &str,
    status_badge: &str,
    status_class: &str,
    status_icon: &str,
    last_updated: &str,
    circuits_html: &str,
    incidents_html: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta name="description" content="{service_name} Status Page - Current service status and incidents">
  <title>{service_name} Status</title>
  <style>{css}</style>
</head>
<body>
  <div class="container">
    <header>
      <div class="logo">{service_name}</div>
      <div class="status-badge {status_class}">
        <span>{status_icon}</span>
        <span>{status_badge}</span>
      </div>
      <div class="last-updated">Last updated: <span data-time="{last_updated}">{last_updated}</span></div>
    </header>

    <section class="section">
      <h2 class="section-title">Components</h2>
      <div class="circuit-list">
        {circuits_html}
      </div>
    </section>

    <section class="section">
      <h2 class="section-title">Incidents</h2>
      {incidents_html}
    </section>

    <footer>
      <p>Powered by <a href="https://ranvier.studio" target="_blank">Ranvier</a></p>
      <p>Generated with ranvier-status</p>
    </footer>
  </div>
  <script>{js}</script>
</body>
</html>"#,
        service_name = service_name,
        status_badge = status_badge,
        status_class = status_class,
        status_icon = status_icon,
        last_updated = last_updated,
        circuits_html = circuits_html,
        incidents_html = incidents_html,
        css = DEFAULT_CSS,
        js = DEFAULT_JS,
    )
}
