import type { Metadata } from "next";
import "./globals.css";

const siteUrl = "https://dopedb-cjs1301.vercel.app";

export const metadata: Metadata = {
  metadataBase: new URL(siteUrl),
  title: "dopedb - Local-first AI database client",
  description:
    "An open-source macOS database client where AI agents can help with SQL while credentials, approvals, and audit logs stay local.",
  openGraph: {
    title: "dopedb",
    description:
      "Talk to your database through an AI agent. Keep credentials, approvals, and audit logs local.",
    url: siteUrl,
    siteName: "dopedb",
    images: [
      {
        url: "/dopedb-dashboard.png",
        width: 1600,
        height: 1120,
        alt: "dopedb desktop app preview",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title: "dopedb",
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
      <body>{children}</body>
    </html>
  );
}
