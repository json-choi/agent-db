import Image from "next/image";
import {
  ArrowRight,
  CheckCircle2,
  Database,
  Download,
  ExternalLink,
  FileClock,
  GitBranch,
  KeyRound,
  LockKeyhole,
  ShieldCheck,
  Sparkles,
  TerminalSquare,
  Waypoints,
} from "lucide-react";

const repoUrl = "https://github.com/json-choi/dopedb";
const releasesUrl = `${repoUrl}/releases/latest`;

const principles = [
  {
    icon: ShieldCheck,
    title: "Read-only first",
    body: "Agent-written SQL is treated as a proposal until dopedb parses, classifies, and enforces the safety policy.",
  },
  {
    icon: KeyRound,
    title: "Credentials stay local",
    body: "Connections and secrets live in the native app boundary instead of being handed directly to the spawned agent.",
  },
  {
    icon: FileClock,
    title: "Auditable by design",
    body: "Queries, approvals, previews, and execution results are recorded so every database action has a trail.",
  },
];

const workflow = [
  "Connect a local, staging, or production database profile.",
  "Ask Codex to inspect schemas, draft SQL, or explain a result.",
  "Let dopedb enforce read/write policy before anything runs.",
  "Approve writes only after rollback preview and risk classification.",
];

const docs = [
  {
    title: "Architecture",
    href: `${repoUrl}/blob/main/ARCHITECTURE.md`,
    body: "Native app, agent bridge, safety layers, and audit pipeline.",
  },
  {
    title: "Build from source",
    href: `${repoUrl}/blob/main/BUILD.md`,
    body: "Tauri, Vite, Rust core, and MCP bridge packaging notes.",
  },
  {
    title: "Roadmap",
    href: `${repoUrl}/blob/main/ROADMAP.md`,
    body: "Prototype, MVP, and v1 milestones for the open-source project.",
  },
];

export default function Home() {
  return (
    <main>
      <header className="topbar">
        <a className="brand" href="#top" aria-label="dopedb home">
          <span className="brand-mark" aria-hidden="true">
            <Database size={18} />
          </span>
          <span>dopedb</span>
        </a>
        <nav className="nav-links" aria-label="Primary navigation">
          <a href="#safety">Safety</a>
          <a href="#download">Download</a>
          <a href="#docs">Docs</a>
        </nav>
        <a className="icon-link" href={repoUrl} aria-label="Open GitHub repository">
          <GitBranch size={20} />
        </a>
      </header>

      <section className="hero" id="top">
        <div className="hero-copy">
          <p className="eyebrow">
            <Sparkles size={15} />
            Open-source local AI database client
          </p>
          <h1>dopedb</h1>
          <p className="hero-text">
            Talk to your database through an AI agent. Keep credentials, write approvals,
            rollback previews, and audit logs inside a native macOS app.
          </p>
          <div className="hero-actions" aria-label="Primary actions">
            <a className="button primary" href={releasesUrl}>
              <Download size={18} />
              Download for macOS
            </a>
            <a className="button secondary" href={repoUrl}>
              <GitBranch size={18} />
              View on GitHub
            </a>
          </div>
          <div className="signal-row" aria-label="Project highlights">
            <span>
              <CheckCircle2 size={16} />
              Tauri native
            </span>
            <span>
              <CheckCircle2 size={16} />
              Codex powered
            </span>
            <span>
              <CheckCircle2 size={16} />
              Local audit trail
            </span>
          </div>
        </div>

        <div className="hero-visual" aria-label="dopedb product preview">
          <div className="preview-shell">
            <Image
              src="/dopedb-dashboard.png"
              alt="dopedb desktop app showing a SQL result, safety gate, and audit timeline"
              width={1600}
              height={1120}
              priority
            />
          </div>
        </div>
      </section>

      <section className="principles" id="safety">
        <div className="section-head">
          <p className="eyebrow">
            <LockKeyhole size={15} />
            Safety boundary
          </p>
          <h2>The agent can suggest. The app enforces.</h2>
          <p>
            dopedb is built for the uncomfortable middle ground: powerful AI help,
            production database caution, and a human who still needs the final say.
          </p>
        </div>
        <div className="feature-grid">
          {principles.map((item) => (
            <article className="feature-card" key={item.title}>
              <item.icon size={22} />
              <h3>{item.title}</h3>
              <p>{item.body}</p>
            </article>
          ))}
        </div>
      </section>

      <section className="workflow">
        <div className="workflow-copy">
          <p className="eyebrow">
            <Waypoints size={15} />
            Daily flow
          </p>
          <h2>Built for database work that needs judgment.</h2>
          <ol className="steps">
            {workflow.map((step) => (
              <li key={step}>{step}</li>
            ))}
          </ol>
        </div>
        <div className="terminal-panel" aria-label="Example agent output">
          <div className="terminal-head">
            <span />
            <span />
            <span />
          </div>
          <pre>
            <code>{`codex -> proposed write

UPDATE customers
SET plan = 'pro'
WHERE id = 1842;

dopedb safety:
  classification: write
  rows estimated: 1
  preview: rollback transaction ready
  approval: required`}</code>
          </pre>
        </div>
      </section>

      <section className="download-band" id="download">
        <div>
          <p className="eyebrow">
            <Download size={15} />
            Download
          </p>
          <h2>Install the latest macOS build from GitHub Releases.</h2>
          <p>
            The first public channel is macOS through GitHub Releases. The app checks the
            signed release feed and enables updates from Settings when a newer version is
            available.
          </p>
          <div className="gatekeeper-note">
            <LockKeyhole size={18} />
            <div>
              <h3>macOS may show a developer warning.</h3>
              <p>
                Until dopedb is notarized with an Apple Developer ID, approve it from
                System Settings, Privacy & Security, Open Anyway after confirming the file
                came from GitHub Releases.
              </p>
            </div>
          </div>
        </div>
        <div className="release-actions">
          <a className="button primary" href={releasesUrl}>
            <Download size={18} />
            Latest release
          </a>
          <a className="button secondary" href={`${repoUrl}/blob/main/BUILD.md`}>
            <TerminalSquare size={18} />
            Build from source
          </a>
        </div>
      </section>

      <section className="docs" id="docs">
        <div className="section-head compact">
          <p className="eyebrow">
            <ExternalLink size={15} />
            Project docs
          </p>
          <h2>Open the internals, not just the binary.</h2>
        </div>
        <div className="docs-grid">
          {docs.map((doc) => (
            <a className="doc-card" href={doc.href} key={doc.title}>
              <span>{doc.title}</span>
              <p>{doc.body}</p>
              <ArrowRight size={18} />
            </a>
          ))}
        </div>
      </section>
    </main>
  );
}
