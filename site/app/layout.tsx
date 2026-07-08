import type { Metadata } from "next";
import { Analytics } from "@vercel/analytics/next";
import "./globals.css";

const siteUrl = "https://dopedb.dev";

export const metadata: Metadata = {
  metadataBase: new URL(siteUrl),
  applicationName: "DopeDB",
  title: {
    default: "DopeDB - Local-first AI database client",
    template: "%s - DopeDB",
  },
  description:
    "An open-source desktop database client where AI agents can help with SQL while credentials, approvals, and audit logs stay local.",
  keywords: [
    "DopeDB",
    "AI database client",
    "MCP database tools",
    "Tauri database client",
    "local-first database client",
    "SQL safety",
  ],
  authors: [{ name: "Jaesong Choi", url: "https://github.com/json-choi" }],
  creator: "Jaesong Choi",
  publisher: "DopeDB",
  category: "Developer Tools",
  alternates: {
    canonical: "/",
  },
  icons: {
    icon: [
      { url: "/favicon.ico", sizes: "any" },
      { url: "/favicon-48x48.png", sizes: "48x48", type: "image/png" },
      { url: "/icon-192.png", sizes: "192x192", type: "image/png" },
      { url: "/icon-512.png", sizes: "512x512", type: "image/png" },
      { url: "/favicon.svg", type: "image/svg+xml" },
    ],
    shortcut: "/favicon.ico",
    apple: [{ url: "/apple-touch-icon.png", sizes: "180x180", type: "image/png" }],
  },
  robots: {
    index: true,
    follow: true,
    googleBot: {
      index: true,
      follow: true,
      "max-image-preview": "large",
      "max-snippet": -1,
      "max-video-preview": -1,
    },
  },
  openGraph: {
    title: "DopeDB",
    description:
      "Talk to your database through an AI agent. Keep credentials, approvals, and audit logs local.",
    url: siteUrl,
    siteName: "DopeDB",
    type: "website",
    locale: "en_US",
    images: [
      {
        url: "/dopedb-dashboard.png",
        width: 1600,
        height: 1120,
        alt: "DopeDB desktop app preview",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title: "DopeDB",
    description:
      "A local-first AI database client with read-only defaults, approval gates, and audit logs.",
    images: ["/dopedb-dashboard.png"],
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>
        {children}
        <Analytics />
      </body>
    </html>
  );
}
