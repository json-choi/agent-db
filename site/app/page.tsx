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
import { TrackedLink } from "./TrackedLink";

const repoUrl = "https://github.com/json-choi/dopedb";
const releasesUrl = `${repoUrl}/releases/latest`;
const siteUrl = "https://dopedb.dev";

type Lang = "en" | "ko";

const copy = {
  en: {
    nav: {
      why: "Why DopeDB",
      safety: "Safety",
      download: "Download",
      docs: "Docs",
      github: "Open GitHub repository",
      home: "DopeDB home",
    },
    hero: {
      eyebrow: "Free local MCP database gateway",
      tag: "Let your agent query data without handing over the keys",
      text:
        "DopeDB gives MCP-capable agents a guarded path into Postgres, MySQL, and SQLite. Keep using your favorite agent for questions and analysis while credentials, read-only enforcement, write approvals, rollback previews, and audit logs stay inside a native macOS app.",
      download: "Download for macOS",
      github: "View on GitHub",
      signals: ["Agent-ready", "Credentials stay local", "Writes stay gated"],
      imageAlt:
        "DopeDB desktop app showing a SQL result, safety gate, and audit timeline",
    },
    positioning: {
      eyebrow: "The gap DopeDB fills",
      title: "Your agent needs database context. It does not need your database keys.",
      body:
        "Database clients help humans operate data. Text-to-SQL tools help generate queries. DopeDB sits between them: a local MCP gateway that lets agents inspect and query databases without raw credentials or silent write access.",
      items: [
        {
          title: "Use the agent you already trust",
          body:
            "Claude Code, Codex, Claude Desktop, or another MCP client can ask DopeDB for schema and read-query context.",
        },
        {
          title: "Expose context, not credentials",
          body:
            "Connections and secrets stay in the local app boundary while the agent gets a narrow, auditable tool surface.",
        },
        {
          title: "Keep risky actions visible",
          body:
            "Read queries can flow. Writes and DDL stay behind policy, approval, rollback preview, and audit history.",
        },
      ],
    },
    principles: {
      eyebrow: "Safety boundary",
      title: "The agent can suggest. The app enforces.",
      body:
        "DopeDB is built for the uncomfortable middle ground: powerful agent workflows, production database caution, and a human who still needs the final say.",
      items: [
        {
          icon: ShieldCheck,
          title: "Read-only first",
          body:
            "Agent-written SQL is treated as a proposal until DopeDB parses, classifies, and enforces the safety policy.",
        },
        {
          icon: KeyRound,
          title: "Credentials stay local",
          body:
            "Connections and secrets live in the native app boundary instead of being handed directly to the spawned agent.",
        },
        {
          icon: FileClock,
          title: "Auditable by design",
          body:
            "Queries, approvals, previews, and execution results are recorded so every database action has a trail.",
        },
      ],
    },
    workflow: {
      eyebrow: "Agent-ready flow",
      title: "Open your database to an agent without opening the floodgates.",
      steps: [
        "Connect a local, staging, or production database profile.",
        "Connect your MCP-capable agent to DopeDB.",
        "Ask it to inspect schemas, draft SQL, or explain a result.",
        "Let DopeDB enforce read/write policy before anything runs.",
        "Review writes only after rollback preview and risk classification.",
      ],
      terminal: `agent -> proposed write

UPDATE customers
SET plan = 'pro'
WHERE id = 1842;

DopeDB safety:
  classification: write
  rows estimated: 1
  preview: rollback transaction ready
  approval: required`,
    },
    download: {
      eyebrow: "Download",
      title: "Install the latest macOS build from GitHub Releases.",
      body:
        "The first public channel is macOS through GitHub Releases. The app checks the signed release feed and enables updates from Settings when a newer version is available.",
      warningTitle: "macOS may show a developer warning.",
      warningBody:
        "Until DopeDB is notarized with an Apple Developer ID, approve it from System Settings, Privacy & Security, Open Anyway after confirming the file came from GitHub Releases.",
      terminalPrefix: "Terminal alternative after copying to Applications:",
      latest: "Latest release",
      source: "Build from source",
    },
    docs: {
      eyebrow: "Project docs",
      title: "Open the internals, not just the binary.",
      items: [
        {
          title: "Project guide",
          href: `${repoUrl}/blob/main/docs/PROJECT.md`,
          body: "Architecture, release flow, safety model, and maintainer notes.",
        },
        {
          title: "Korean README",
          href: `${repoUrl}/blob/main/README.md`,
          body: "Install, develop, release, and update details in Korean.",
        },
        {
          title: "Releases",
          href: releasesUrl,
          body: "Latest macOS downloads and updater metadata.",
        },
      ],
    },
    jsonDescription:
      "DopeDB is a free local MCP database gateway for AI agents. Agents can inspect schemas and run read queries while credentials, write approvals, rollback previews, and audit logs stay in a native macOS app.",
  },
  ko: {
    nav: {
      why: "왜 DopeDB인가",
      safety: "안전",
      download: "다운로드",
      docs: "문서",
      github: "GitHub 저장소 열기",
      home: "DopeDB 홈",
    },
    hero: {
      eyebrow: "무료 로컬 MCP 데이터베이스 게이트웨이",
      tag: "키를 넘기지 않고 에이전트에게 데이터 통로를 열어주세요",
      text:
        "DopeDB는 MCP 지원 에이전트가 Postgres, MySQL, SQLite를 안전하게 읽고 이해할 수 있게 합니다. 질문과 분석은 좋아하는 에이전트에게 맡기고, 인증 정보, 읽기 전용 실행, 쓰기 승인, 롤백 미리보기, 감사 로그는 네이티브 macOS 앱 안에 둡니다.",
      download: "macOS용 다운로드",
      github: "GitHub에서 보기",
      signals: ["에이전트 준비 완료", "인증 정보는 로컬에", "쓰기는 승인 뒤에"],
      imageAlt:
        "SQL 결과, 안전 게이트, 감사 타임라인을 보여주는 DopeDB 데스크톱 앱",
    },
    positioning: {
      eyebrow: "DopeDB가 메우는 빈틈",
      title: "에이전트에게 필요한 건 DB 맥락이지, DB 키가 아닙니다.",
      body:
        "데이터베이스 클라이언트는 사람이 데이터를 다루게 해주고, text-to-SQL 도구는 쿼리를 만들어줍니다. DopeDB는 그 사이에 있습니다. 에이전트가 데이터베이스를 읽고 쿼리할 수 있게 하되, 원본 인증 정보와 조용한 쓰기 권한은 넘기지 않는 로컬 MCP 게이트웨이입니다.",
      items: [
        {
          title: "이미 믿는 에이전트를 그대로",
          body:
            "Claude Code, Codex, Claude Desktop 같은 MCP 클라이언트가 DopeDB를 통해 스키마와 읽기 쿼리 맥락을 요청할 수 있습니다.",
        },
        {
          title: "인증 정보가 아닌 맥락만 노출",
          body:
            "연결과 비밀값은 로컬 앱 경계 안에 두고, 에이전트에는 좁고 감사 가능한 도구 표면만 제공합니다.",
        },
        {
          title: "위험한 동작은 보이는 곳에",
          body:
            "읽기 쿼리는 자연스럽게 흐르게 두고, 쓰기와 DDL은 정책, 승인, 롤백 미리보기, 감사 기록 뒤에 둡니다.",
        },
      ],
    },
    principles: {
      eyebrow: "안전 경계",
      title: "에이전트는 제안하고, 앱은 집행합니다.",
      body:
        "DopeDB는 강력한 에이전트 워크플로우, 프로덕션 데이터베이스에 대한 조심스러움, 그리고 최종 결정을 내려야 하는 사람 사이의 불편하지만 중요한 지점을 위해 만들어졌습니다.",
      items: [
        {
          icon: ShieldCheck,
          title: "읽기 전용 우선",
          body:
            "에이전트가 작성한 SQL은 DopeDB가 파싱하고 분류하고 안전 정책을 적용하기 전까지 제안으로 취급됩니다.",
        },
        {
          icon: KeyRound,
          title: "인증 정보는 로컬에",
          body:
            "연결 정보와 비밀값은 생성된 에이전트에 직접 넘기지 않고 네이티브 앱 경계 안에 둡니다.",
        },
        {
          icon: FileClock,
          title: "감사 가능한 설계",
          body:
            "쿼리, 승인, 미리보기, 실행 결과를 기록해 모든 데이터베이스 작업에 추적 가능한 흔적을 남깁니다.",
        },
      ],
    },
    workflow: {
      eyebrow: "에이전트 준비 흐름",
      title: "데이터베이스를 에이전트에게 열되, 수문까지 열지는 않습니다.",
      steps: [
        "로컬, 스테이징, 프로덕션 데이터베이스 프로필을 연결합니다.",
        "MCP 지원 에이전트를 DopeDB에 연결합니다.",
        "스키마 확인, SQL 초안 작성, 결과 설명을 요청합니다.",
        "무엇이든 실행되기 전에 DopeDB가 읽기/쓰기 정책을 적용합니다.",
        "롤백 미리보기와 위험 분류를 확인한 뒤에만 쓰기를 검토합니다.",
      ],
      terminal: `agent -> proposed write

UPDATE customers
SET plan = 'pro'
WHERE id = 1842;

DopeDB safety:
  classification: write
  rows estimated: 1
  preview: rollback transaction ready
  approval: required`,
    },
    download: {
      eyebrow: "다운로드",
      title: "최신 macOS 빌드를 GitHub Releases에서 설치하세요.",
      body:
        "첫 공개 채널은 GitHub Releases를 통한 macOS 빌드입니다. 앱은 서명된 릴리스 피드를 확인하고, 새 버전이 있으면 Settings에서 업데이트할 수 있게 합니다.",
      warningTitle: "macOS 개발자 확인 경고가 표시될 수 있습니다.",
      warningBody:
        "Apple Developer ID로 공증되기 전까지는 GitHub Releases에서 받은 파일인지 확인한 뒤 System Settings, Privacy & Security, Open Anyway에서 실행을 허용하세요.",
      terminalPrefix: "Applications로 복사한 뒤 터미널에서 실행할 수 있는 대안:",
      latest: "최신 릴리스",
      source: "소스에서 빌드",
    },
    docs: {
      eyebrow: "프로젝트 문서",
      title: "바이너리뿐 아니라 내부 구조까지 열어둡니다.",
      items: [
        {
          title: "프로젝트 가이드",
          href: `${repoUrl}/blob/main/docs/PROJECT.md`,
          body: "아키텍처, 릴리스 흐름, 안전 모델, 메인테이너 노트.",
        },
        {
          title: "English README",
          href: `${repoUrl}/blob/main/README.en.md`,
          body: "설치, 개발, 릴리스, 업데이트 정보를 영어로 확인합니다.",
        },
        {
          title: "릴리스",
          href: releasesUrl,
          body: "최신 macOS 다운로드와 updater metadata.",
        },
      ],
    },
    jsonDescription:
      "DopeDB는 AI 에이전트를 위한 무료 로컬 MCP 데이터베이스 게이트웨이입니다. 에이전트는 스키마를 확인하고 읽기 쿼리를 실행할 수 있고, 인증 정보, 쓰기 승인, 롤백 미리보기, 감사 로그는 네이티브 macOS 앱에 남습니다.",
  },
};

function normalizeLang(value: string | string[] | undefined): Lang {
  const lang = Array.isArray(value) ? value[0] : value;
  return lang === "ko" ? "ko" : "en";
}

export default async function Home({
  searchParams,
}: {
  searchParams?: Promise<Record<string, string | string[] | undefined>>;
}) {
  const params = searchParams ? await searchParams : {};
  const lang = normalizeLang(params.lang);
  const c = copy[lang];
  const otherLang = lang === "ko" ? "en" : "ko";

  const jsonLd = {
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    name: "DopeDB",
    applicationCategory: "DeveloperApplication",
    operatingSystem: "macOS",
    description: c.jsonDescription,
    url: siteUrl,
    downloadUrl: releasesUrl,
    codeRepository: repoUrl,
    image: `${siteUrl}/dopedb-dashboard.png`,
    license: `${repoUrl}/blob/main/LICENSE`,
    inLanguage: lang,
    offers: {
      "@type": "Offer",
      price: "0",
      priceCurrency: "USD",
    },
  };

  return (
    <main lang={lang}>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(jsonLd) }}
      />
      <header className="topbar">
        <a className="brand" href="#top" aria-label={c.nav.home}>
          <span className="brand-mark" aria-hidden="true">
            <Database size={18} />
          </span>
          <span>DopeDB</span>
        </a>
        <nav className="nav-links" aria-label="Primary navigation">
          <a href="#why">{c.nav.why}</a>
          <a href="#safety">{c.nav.safety}</a>
          <a href="#download">{c.nav.download}</a>
          <a href="#docs">{c.nav.docs}</a>
        </nav>
        <div className="topbar-actions">
          <a
            className="language-link"
            href={`/?lang=${otherLang}`}
            hrefLang={otherLang}
            aria-label={otherLang === "ko" ? "한국어로 보기" : "View in English"}
          >
            {otherLang === "ko" ? "한국어" : "English"}
          </a>
          <a className="icon-link" href={repoUrl} aria-label={c.nav.github}>
            <GitBranch size={20} />
          </a>
        </div>
      </header>

      <section className="hero" id="top">
        <div className="hero-copy">
          <p className="eyebrow">
            <Sparkles size={15} />
            {c.hero.eyebrow}
          </p>
          <h1>
            DopeDB
            <span className="hero-title-tag">{c.hero.tag}</span>
          </h1>
          <p className="hero-text">{c.hero.text}</p>
          <div className="hero-actions" aria-label="Primary actions">
            <TrackedLink
              className="button primary"
              href={releasesUrl}
              event="Download Clicked"
              properties={{ source: "hero", target: "github_releases_latest" }}
            >
              <Download size={18} />
              {c.hero.download}
            </TrackedLink>
            <a className="button secondary" href={repoUrl}>
              <GitBranch size={18} />
              {c.hero.github}
            </a>
          </div>
          <div className="signal-row" aria-label="Project highlights">
            {c.hero.signals.map((signal) => (
              <span key={signal}>
                <CheckCircle2 size={16} />
                {signal}
              </span>
            ))}
          </div>
        </div>

        <div className="hero-visual" aria-label="DopeDB product preview">
          <div className="preview-shell">
            <Image
              src="/dopedb-dashboard.png"
              alt={c.hero.imageAlt}
              width={1600}
              height={1120}
              priority
            />
          </div>
        </div>
      </section>

      <section className="positioning" id="why">
        <div className="section-head">
          <p className="eyebrow">
            <Sparkles size={15} />
            {c.positioning.eyebrow}
          </p>
          <h2>{c.positioning.title}</h2>
          <p>{c.positioning.body}</p>
        </div>
        <div className="positioning-grid">
          {c.positioning.items.map((item) => (
            <article className="positioning-item" key={item.title}>
              <h3>{item.title}</h3>
              <p>{item.body}</p>
            </article>
          ))}
        </div>
      </section>

      <section className="principles" id="safety">
        <div className="section-head">
          <p className="eyebrow">
            <LockKeyhole size={15} />
            {c.principles.eyebrow}
          </p>
          <h2>{c.principles.title}</h2>
          <p>{c.principles.body}</p>
        </div>
        <div className="feature-grid">
          {c.principles.items.map((item) => (
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
            {c.workflow.eyebrow}
          </p>
          <h2>{c.workflow.title}</h2>
          <ol className="steps">
            {c.workflow.steps.map((step) => (
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
            <code>{c.workflow.terminal}</code>
          </pre>
        </div>
      </section>

      <section className="download-band" id="download">
        <div>
          <p className="eyebrow">
            <Download size={15} />
            {c.download.eyebrow}
          </p>
          <h2>{c.download.title}</h2>
          <p>{c.download.body}</p>
          <div className="gatekeeper-note">
            <LockKeyhole size={18} />
            <div>
              <h3>{c.download.warningTitle}</h3>
              <p>{c.download.warningBody}</p>
              <p>
                {c.download.terminalPrefix}{" "}
                <code>sudo xattr -dr com.apple.quarantine /Applications/DopeDB.app</code>
              </p>
            </div>
          </div>
        </div>
        <div className="release-actions">
          <TrackedLink
            className="button primary"
            href={releasesUrl}
            event="Download Clicked"
            properties={{ source: "download_section", target: "github_releases_latest" }}
          >
            <Download size={18} />
            {c.download.latest}
          </TrackedLink>
          <a className="button secondary" href={`${repoUrl}/blob/main/docs/PROJECT.md#development`}>
            <TerminalSquare size={18} />
            {c.download.source}
          </a>
        </div>
      </section>

      <section className="docs" id="docs">
        <div className="section-head compact">
          <p className="eyebrow">
            <ExternalLink size={15} />
            {c.docs.eyebrow}
          </p>
          <h2>{c.docs.title}</h2>
        </div>
        <div className="docs-grid">
          {c.docs.items.map((doc) => {
            const content = (
              <>
                <span>{doc.title}</span>
                <p>{doc.body}</p>
                <ArrowRight size={18} />
              </>
            );

            if (doc.href === releasesUrl) {
              return (
                <TrackedLink
                  className="doc-card"
                  href={doc.href}
                  key={doc.title}
                  event="Download Clicked"
                  properties={{ source: "docs_card", target: "github_releases_latest" }}
                >
                  {content}
                </TrackedLink>
              );
            }

            return (
              <a className="doc-card" href={doc.href} key={doc.title}>
                {content}
              </a>
            );
          })}
        </div>
      </section>
    </main>
  );
}
