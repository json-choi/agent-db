import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "DopeDB Workspace",
  description: "Identity and workspace control plane for DopeDB",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="ko">
      <body>{children}</body>
    </html>
  );
}
