import type { MetadataRoute } from "next";

const siteUrl = "https://dopedb.dev";

const aiSearchAndRetrievalCrawlers = [
  "OAI-SearchBot",
  "ChatGPT-User",
  "Claude-SearchBot",
  "Claude-User",
  "PerplexityBot",
  "Perplexity-User",
];

const aiTrainingCrawlers = [
  "GPTBot",
  "ClaudeBot",
  "Google-Extended",
];

export default function robots(): MetadataRoute.Robots {
  return {
    rules: [
      {
        userAgent: aiSearchAndRetrievalCrawlers,
        allow: "/",
      },
      {
        userAgent: aiTrainingCrawlers,
        allow: "/",
      },
      {
        userAgent: "*",
        allow: "/",
      },
    ],
    sitemap: `${siteUrl}/sitemap.xml`,
    host: siteUrl,
  };
}
